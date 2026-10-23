"""Hold every workflow to the release's rules (design/phase5-release.md P5.17), on every pull request and before any
release run is dispatched.

    python scripts/ci/check_workflows.py [--root DIR]

A release run holds PyPI's token, the registry's write permission and, on a tag, the update-signing key, and it runs
test.yml's and package.yml's jobs inside it. What these rules prevent is a workflow that publishes from a pull request
or a dry run, a job that waits for an approval or creates an environment by naming it, code that can change under a
pinned job, and a secret that a build or an installer could read. Each rule is checked from the files alone:

1. every workflow parses (PyYAML reads the ``on`` key as ``True``; this script knows), and sets
   ``permissions: contents: read`` at the top;
2. a job **publishes** when it names an environment, asks for a write permission (``id-token``, ``pages``,
   ``packages``, ``contents``, ``attestations``, ``deployments``, ``pull-requests`` or any other), reads a secret
   other than ``GITHUB_TOKEN``, uses a publishing action (``pypa/gh-action-pypi-publish``,
   ``softprops/action-gh-release``, ``actions/attest-build-provenance``, ``actions/attest``, ``actions/deploy-pages``,
   ``docker/login-action``, or ``docker/build-push-action`` with ``push``) or runs a publishing command (``gh
   release`` create, upload, edit or delete; a mutating ``gh api``; ``gh workflow run`` and gh's other writes;
   ``twine upload``; ``docker push``; ``imagetools create``; ``git push``; ``npm``, ``cargo``, ``uv`` or ``maturin``
   publishing). A publishing job is either in release.yml, with the tag guard ``github.event_name == 'push' &&
   startsWith(github.ref, 'refs/tags/v')`` as conjuncts of its job-level ``if:``, or in docs.yml, with 2-06b's guard
   ``github.ref == 'refs/heads/main'`` as a conjunct of its ``if:`` or, as the ``wiki`` job has it, of exactly the
   steps that read a secret or push. A guard is weakened by being one side of an ``||`` and by ``always()``,
   ``cancelled()`` or ``failure()`` anywhere in the condition, and neither is allowed. No other workflow publishes;
3. package.yml and test.yml publish nothing, and name no environment and no secret (``GITHUB_TOKEN`` included, as
   ``secrets.GITHUB_TOKEN`` or ``github.token``);
4. a called workflow's (``on: workflow_call``) concurrency group contains ``github.workflow`` and, evaluated as its
   caller runs it, differs from its caller's;
5. no ``run:`` block interpolates ``inputs.*``, ``github.event.inputs.*``, ``github.head_ref`` or the pull
   request's or commit's own text: they reach the shell through ``env:``;
6. every maturin-action step pins ``maturin-version: ${{ env.MATURIN_VERSION }}``, which the workflow defines;
7. every job of package.yml and test.yml runs ``scripts/release.py prepare`` after its checkout and before its first
   build or test step (only the checkout, ``setup-*``, cache and download actions may come between), unconditionally;
8. release.yml has no build step (cargo, maturin, PyInstaller, ISCC, ``python -m build``, ``pip wheel``,
   ``docker build`` or ``buildx build``, maturin-action, build-push-action), and no run step in a shell whose
   commands this check cannot read (anything but bash, sh, pwsh, powershell and cmd: ``shell: python``);
9. no upload path can include a ``*.key`` file or the throwaway key folder (a path whose last part could match a
   key's name, a ``throwaway`` folder, the runner's temporary folder whole, or a path with ``..``);
10. release.yml calls neither nightly.yml nor docs.yml (as a called workflow or by ``gh workflow run``), and no
    workflow runs ``build_wiki.py --push``;
11. every remote ``uses:`` (a step's or a job's) is pinned to a 40-character commit SHA, with its version in a comment
    on the same line (``@<sha> # v7.0.1``); ``./`` references are exempt, and a ``docker://`` image needs a
    ``@sha256:`` digest. So does every other image a workflow runs, each matrix leg's: a job's ``container:`` and
    ``services:``, and the one a maturin-action step builds in on Linux, its ``container:`` (``off`` builds on the
    runner); a step that names none while its ``manylinux`` asks for a container (``publish`` and a cross target
    ask for ``auto``) would build in maturin-action's own image, by a moving tag, and is refused. A script's
    ``docker run`` is not read, since its words name no image reliably; so an image a script runs is written where
    this reads it, as a matrix leg's ``container`` or ``image`` or a matrix or ``env:`` key ending in either
    (``FEDORA_IMAGE``), and each such value that is not an expression needs its digest too (5-03).
    ``scripts/ci/action_pins.py verify`` proves the action pins against GitHub;
12. a job that calls a workflow passes no ``secrets:`` (never ``inherit``) and sets ``permissions: contents: read``;
13. artifact names: package.yml uploads only ``pkg-...``, release.yml only ``rel-...``, and no other workflow either
    prefix; the upload names, each matrix leg's, are unique across test.yml, package.yml and release.yml; every
    download in package.yml and release.yml names one artifact, or the pattern ``pkg-*`` or ``rel-*`` and no other;
14. environments: release.yml names ``pypi`` (in its ``publish-pypi`` job only) or, for ``helper-signing`` only,
    the tag expression ``${{ github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') &&
    'helper-signing' || '' }}``, which names nothing on a dispatch; docs.yml names ``github-pages`` (in its
    ``deploy`` job only); no other workflow names one;
15. nothing installs or builds where power is held: a step that reads a secret other than ``GITHUB_TOKEN`` runs no
    package tool (cargo, pip, pipx, npm, npx, uv, maturin) and is not a ``uses:`` step; in release.yml, a job that
    reads such a secret, or holds ``id-token: write`` or any write permission, runs no package tool or ``docker
    build``, uses only ``actions/checkout`` (with ``persist-credentials: false``), ``actions/download-artifact``,
    ``actions/upload-artifact`` and rule 2's publishing actions, and no ``setup-*`` action. Neither such a step nor
    such a job runs a step in a shell whose commands this check cannot read;
16. **release.yml's order and permissions** (P5.11's table, package 5-09): its jobs are exactly the table's
    (``RELEASE_JOBS``), each with its direct ``needs`` (``meta`` besides, where it reads its outputs), so
    ``publish-pypi`` and ``publish-image`` need ``attest-files``, which needs ``check-signature``, and ``image-tags``
    needs ``github-release``; the tag-only jobs carry the tag guard, ``dry-run-summary`` ``github.event_name ==
    'workflow_dispatch'``, and ``meta``, ``tests`` and ``package`` no condition; each job's write permissions are
    exactly the table's; ``sign-helper-manifest`` names ``helper-signing`` through the tag expression and
    ``publish-pypi`` names ``pypi``; the signing job reads its one secret, ``CITAR_UPDATE_SIGNING_KEY``, in one run
    step whose ``env:`` holds it alone and runs ``release_sign.py sign``; ``github-release`` alone runs ``gh
    release`` writes, every ``gh release create`` with ``--draft``, and makes the release public (``--draft=false``)
    only in a step after every create and upload; ``publish-image`` pushes and joins only tags naming the version
    (never ``latest``), and only it and ``image-tags`` push; ``attest-files`` attests (``subject-path``) the folder
    its ``pkg-release-assets`` download fills, after ``assemble_release.py final`` has finished that folder with the
    sums and the signature from ``rel-release-sums``; ``attest-image`` pushes its attestation to the registry for
    ``publish-image``'s digest; and the workflow's concurrency is ``release-${{ github.ref }}``, never cancelled.

Shell commands are read word by word, a redirection (``2>&1``) and a launcher (``sudo -E``, ``env``, ``timeout``)
seen through, and ``pip3.12`` read as pip.
Quoted text, comments and heredocs are data, so ``echo "run build_wiki.py --push"`` runs nothing and a ``<<`` in
quotes starts no heredoc. A script given to a shell is read as commands: the string of ``bash -c`` (``-lc``) or
``eval``, a here-string (``bash <<< '...'``), ``pwsh -Command`` and ``cmd /c``'s words, and a heredoc a shell reads
(``bash <<EOF``, ``bash -s <<'EOF'``, ``cat <<EOF | bash``). A script written to a file and run later is not read;
the permission and secret rules hold there. Every finding names its file, its job where it has one, and its rule;
exit status 0 when every rule holds, 1 when any does not, 2 when the check cannot run (no PyYAML, no workflows
folder).
"""
from __future__ import annotations

import argparse
import fnmatch
import itertools
import re
import sys
from dataclasses import dataclass
from pathlib import Path

try:
    import yaml
except ImportError:  # the lint job installs it; uvicorn[standard] brings it to every suite environment
    yaml = None

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = Path(".github") / "workflows"

RELEASE, DOCS, TESTS, PACKAGE = "release.yml", "docs.yml", "test.yml", "package.yml"
#: The workflows whose upload names share a release run's one artifact namespace (P5.3.1).
SHARED_NAMESPACE = (TESTS, PACKAGE, RELEASE)
#: Each workflow's own artifact prefix; no other workflow may upload under either.
PREFIXES = {PACKAGE: "pkg-", RELEASE: "rel-"}
#: The patterns a download in package.yml or release.yml may take instead of a name.
DOWNLOAD_PATTERNS = ("pkg-*", "rel-*")

PUBLISHING_ACTIONS = ("pypa/gh-action-pypi-publish", "softprops/action-gh-release", "actions/attest-build-provenance",
                      "actions/attest", "actions/deploy-pages", "docker/login-action")
#: Actions that may come between the checkout and release.py prepare: they set up, restore or fetch, and build
#: or test nothing.
SETUP_ACTIONS = ("actions/checkout", "actions/cache", "actions/download-artifact", "swatinem/rust-cache",
                 "taiki-e/install-action")
#: What a release.yml job holding power may use, besides rule 2's publishing actions.
POWER_ALLOWED = ("actions/checkout", "actions/download-artifact", "actions/upload-artifact")
PACKAGE_TOOLS = {"cargo", "pip", "pipx", "npm", "npx", "uv", "uvx", "maturin"}  # tool_of names pip3.12 pip
#: The shells whose run blocks this check reads. A run step in another (``shell: python``) cannot be read, so it may
#: not run where rule 8 or 15 must see its commands.
READABLE_SHELLS = ("bash", "sh", "pwsh", "powershell", "cmd")
#: The environments release.yml may name through the tag expression (P5.11; P5.18.2 allows github-pages, pypi and
#: this one, and a misspelt name would create a new environment, with no reviewer and no secret, at the tag push).
TAG_ENVIRONMENTS = ("helper-signing",)
#: Expressions a run block may not interpolate (rule 5): a dispatch's inputs and text a pull request's author writes.
TAINTED = re.compile(
    r"\binputs\s*\.|\bgithub\s*\.\s*event\s*\.\s*inputs\b|\bgithub\s*\.\s*head_ref\b"
    r"|\bgithub\s*\.\s*event\s*\.\s*pull_request\s*\.\s*(?:title|body|head\s*\.\s*(?:ref|label))\b"
    r"|\bgithub\s*\.\s*event\s*\.\s*head_commit\s*\.\s*(?:message|author)\b",
    re.IGNORECASE)
EXPR = re.compile(r"\$\{\{(.*?)\}\}", re.DOTALL)
SHA40 = re.compile(r"[0-9a-f]{40}")
#: An image named by its content: ``registry/name@sha256:<64 hex>``.
DIGEST = re.compile(r"@sha256:[0-9a-f]{64}$")
#: maturin-action's ``container:`` values that name no image: each takes one from the action's own table.
MATURIN_TABLE_IMAGE = ("on", "auto", "true")
VERSION_COMMENT = re.compile(r"v?\d+(?:\.\d+)*(?:[-+][0-9A-Za-z.-]+)?")
USES_LINE = re.compile(r"""^\s*(?:-\s+)?uses:\s*(["']?)(?P<ref>[^\s"'#]+)\1\s*(?:#\s*(?P<comment>.*))?$""")


@dataclass(frozen=True)
class Finding:
    file: str
    job: str | None
    rule: int
    message: str

    def __str__(self) -> str:
        job = f"job {self.job}: " if self.job else ""
        return f"{self.file}: {job}rule {self.rule}: {self.message}"


# ================================================================================================ expressions
# GitHub's expression language, enough to read a condition's structure: literals, contexts, calls, !, comparisons,
# && and ||. The tree is tuples: ("lit", v), ("ctx", "a.b.c"), ("call", name, [args]), ("not", x),
# ("cmp", op, a, b), ("and", [xs]), ("or", [xs]). Contexts and function names are case-insensitive, so lowercased.

TOKEN = re.compile(r"\s*(?:(?P<str>'(?:[^']|'')*')|(?P<num>-?\d+(?:\.\d+)?(?![\w-]))|(?P<op>&&|\|\||==|!=|<=|>=|[<>!()\[\],.*])"
                   r"|(?P<name>[A-Za-z_][A-Za-z0-9_-]*))")


class ExprError(ValueError):
    pass


def tokens(text: str) -> list[tuple[str, str]]:
    out, at = [], 0
    while at < len(text):
        if text[at:].strip() == "":
            break
        match = TOKEN.match(text, at)
        if not match or match.end() == at:
            raise ExprError(f"cannot read {text[at:]!r}")
        kind = match.lastgroup
        out.append((kind, match[kind]))
        at = match.end()
    return out


def parse_expr(text: str):
    toks = tokens(text)
    pos = 0

    def peek(value=None):
        if pos < len(toks) and (value is None or toks[pos][1] == value):
            return toks[pos]
        return None

    def take(value=None):
        nonlocal pos
        tok = peek(value)
        if tok is None:
            raise ExprError(f"expected {value or 'more'} in {text!r}")
        pos += 1
        return tok

    def chain(kind, op, sub):
        items = [sub()]
        while peek(op):
            take(op)
            items.append(sub())
        return items[0] if len(items) == 1 else (kind, items)

    def p_or():
        return chain("or", "||", p_and)

    def p_and():
        return chain("and", "&&", p_eq)

    def p_eq():
        left = p_rel()
        while peek("==") or peek("!="):
            op = take()[1]
            left = ("cmp", op, left, p_rel())
        return left

    def p_rel():
        left = p_unary()
        while any(peek(op) for op in ("<", "<=", ">", ">=")):
            op = take()[1]
            left = ("cmp", op, left, p_unary())
        return left

    def p_unary():
        if peek("!"):
            take("!")
            return ("not", p_unary())
        return p_postfix()

    def p_postfix():
        node = p_primary()
        while True:
            if peek("."):
                take(".")
                name = take()[1]
                node = ("ctx", f"{node[1]}.{name.lower()}") if node[0] == "ctx" else ("ctx", "?")
            elif peek("["):
                take("[")
                index = p_or()
                take("]")
                if node[0] == "ctx" and index[0] == "lit" and isinstance(index[1], str):
                    node = ("ctx", f"{node[1]}.{index[1].lower()}")
                else:
                    node = ("ctx", f"{node[1] if node[0] == 'ctx' else '?'}.?")
            else:
                return node

    def p_primary():
        kind, value = take()
        if kind == "str":
            return ("lit", value[1:-1].replace("''", "'"))
        if kind == "num":
            return ("lit", float(value))
        if value == "(":
            node = p_or()
            take(")")
            return node
        if kind == "name":
            low = value.lower()
            if low in ("true", "false"):
                return ("lit", low == "true")
            if low == "null":
                return ("lit", None)
            if peek("("):
                take("(")
                args = []
                if not peek(")"):
                    args.append(p_or())
                    while peek(","):
                        take(",")
                        args.append(p_or())
                take(")")
                return ("call", low, args)
            return ("ctx", low)
        if value == "*":
            return ("ctx", "*")
        raise ExprError(f"unexpected {value!r} in {text!r}")

    node = p_or()
    if pos != len(toks):
        raise ExprError(f"unexpected {toks[pos][1]!r} in {text!r}")
    return node


def condition(value):
    """An ``if:`` or ``environment:`` value as an expression tree; None when it is not one expression (literal text
    around a ``${{ }}``, or something this reader cannot parse), which no guard accepts."""
    if isinstance(value, bool):
        return ("lit", value)
    if not isinstance(value, str):
        return None
    text = value.strip()
    match = re.fullmatch(r"\$\{\{(.*)\}\}", text, re.DOTALL)
    if match and "${{" not in match[1] and "}}" not in match[1]:
        text = match[1]
    elif "${{" in text:
        return None
    try:
        return parse_expr(text)
    except ExprError:
        return None


def walk(node):
    yield node
    if node[0] in ("and", "or"):
        for item in node[1]:
            yield from walk(item)
    elif node[0] == "not":
        yield from walk(node[1])
    elif node[0] == "cmp":
        yield from walk(node[2])
        yield from walk(node[3])
    elif node[0] == "call":
        for arg in node[2]:
            yield from walk(arg)


def conjuncts(node) -> list:
    """The terms an expression requires all of: ``(a && b) && c`` is a, b and c."""
    return [part for item in node[1] for part in conjuncts(item)] if node[0] == "and" else [node]


def is_equal(node, ctx: str, literal: str) -> bool:
    if node[0] != "cmp" or node[1] != "==":
        return False
    pair = {node[2], node[3]}
    return pair == {("ctx", ctx), ("lit", literal)}


def is_tag_ref(node) -> bool:
    return (node[0] == "call" and node[1] == "startswith" and len(node[2]) == 2 and node[2][0] == ("ctx", "github.ref")
            and node[2][1][0] == "lit" and isinstance(node[2][1][1], str) and node[2][1][1].startswith("refs/tags/v"))


WEAKENERS = ("always", "cancelled", "failure")
TAG_GUARD_TEXT = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"


def guard_problem(value, kind: str) -> str | None:
    """None when ``value`` (an ``if:``) carries the guard of ``kind`` ("tag" or "main") unweakened; else why not."""
    if value is None:
        needed = "github.ref == 'refs/heads/main'" if kind == "main" else TAG_GUARD_TEXT
        return f"has no if: (it needs {needed})"
    node = condition(value)
    if node is None:
        return f"its if: ({value!r}) is not one expression this check can read"
    weak = sorted({n[1] for n in walk(node) if n[0] == "call" and n[1] in WEAKENERS})
    if weak:
        return f"its if: runs under {', '.join(w + '()' for w in weak)}, which weakens the guard"
    parts = conjuncts(node)
    if kind == "main":
        if not any(is_equal(p, "github.ref", "refs/heads/main") for p in parts):
            return "its if: lacks github.ref == 'refs/heads/main' as a conjunct (an || or a missing check)"
        return None
    missing = []
    if not any(is_equal(p, "github.event_name", "push") for p in parts):
        missing.append("github.event_name == 'push'")
    if not any(is_tag_ref(p) for p in parts):
        missing.append("startsWith(github.ref, 'refs/tags/v')")
    if missing:
        return f"its if: lacks {' and '.join(missing)} as a conjunct (an || or a missing check; a dispatch on a tag " \
               f"ref must not publish)"
    return None


def tag_expression_name(value) -> str | None:
    """The environment a value names through the tag expression, or None when it is not that expression."""
    node = condition(value) if isinstance(value, str) and "${{" in value else None
    if node is None or node[0] != "or" or len(node[1]) != 2 or node[1][1] != ("lit", ""):
        return None
    left = node[1][0]
    if left[0] != "and":
        return None
    *guards, last = conjuncts(left)
    if last[0] != "lit" or not isinstance(last[1], str) or not last[1]:
        return None
    if any(n[0] == "call" and n[1] in WEAKENERS for g in guards for n in walk(g)):
        return None
    if not any(is_equal(g, "github.event_name", "push") for g in guards) or not any(is_tag_ref(g) for g in guards):
        return None
    return last[1]


# ================================================================================================ shell
# A run block read as shell, word by word: quotes removed, comments dropped, simple commands split at ; & | ( ) `
# and newlines, each remembering the operator that ended it so that a pipeline can be followed. ``${{ }}``
# expressions become a placeholder word first. A heredoc's body is data unless a shell reads it as its script.

SEPARATORS = set(";&|()`\n")
KEYWORDS = {"if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "time", "case", "esac"}
WRAPPERS = {"nohup", "exec", "command", "builtin"}
#: sudo's options whose next word is their value: ``sudo -u runner pip install`` runs pip.
SUDO_VALUES = {"-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "-U", "--user", "--group", "--close-from",
               "--chdir", "--host", "--prompt", "--role", "--type", "--command-timeout", "--other-user"}
#: The POSIX shells, whose ``-c`` string, here-string and heredoc can be a script.
SHELLS = {"bash", "sh", "zsh", "dash", "ksh"}
#: A shell's options whose next word is their value (``bash -o pipefail``).
SHELL_VALUES = {"-o", "+o", "-O", "+O", "--rcfile", "--init-file"}
#: A heredoc's delimiter, quotes removed; anything else after ``<<`` (``$((1 << 2))``) starts no heredoc.
DELIMITER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
#: A redirection word (``>``, ``2>&1``, ``&>log``, ``<file``); an empty group 1 means its target is the next word.
REDIRECTION = re.compile(r"\d*(?:&>>?|>&|<&|[<>]{1,2})(.*)", re.DOTALL)


def heredoc_delimiter(text: str, at: int) -> tuple[str | None, int]:
    """The delimiter of a heredoc whose ``<<`` ends just before ``at`` (quotes removed) and where its word ends; None
    and ``at`` itself when no heredoc starts there."""
    start = at
    if at < len(text) and text[at] == "-":  # <<- strips leading tabs, which the comparison below ignores anyway
        at += 1
    while at < len(text) and text[at] in " \t":
        at += 1
    out = []
    while at < len(text) and text[at] not in " \t\r\n;&|()<>`":
        ch = text[at]
        if ch in "'\"":
            close = text.find(ch, at + 1)
            close = len(text) if close < 0 else close
            out.append(text[at + 1:close])
            at = close + 1
        elif ch == "\\" and at + 1 < len(text):
            out.append(text[at + 1])
            at += 2
        else:
            out.append(ch)
            at += 1
    delimiter = "".join(out)
    return (delimiter, at) if DELIMITER.fullmatch(delimiter) else (None, start)


def heredoc_body(text: str, start: int, delimiter: str) -> tuple[str, int]:
    """A heredoc's body from ``start`` up to its delimiter's line, and the index of the newline that ends that line
    (the text's end when the delimiter never comes: bash, too, then reads the rest as the body)."""
    lines, at = [], start
    while at < len(text):
        newline = text.find("\n", at)
        newline = len(text) if newline < 0 else newline
        line = text[at:newline]
        if line.strip() == delimiter:  # YAML has taken the block's indentation; <<- would take the tabs
            return "\n".join(lines), newline
        lines.append(line)
        at = newline + 1
    return "\n".join(lines), len(text)


def without_redirections(argv: list[str]) -> list[str]:
    out, skip = [], False
    for w in argv:
        if skip:
            skip = False
            continue
        match = REDIRECTION.fullmatch(w)
        if match and w != "<<<":
            skip = match[1] == ""
            continue
        out.append(w)
    return out


def shell_reads_stdin(argv: list[str]) -> bool:
    """Whether a command is a shell running the script it reads on stdin: ``bash``, ``bash -e``, ``sh -s arg``, but
    not ``bash x.sh`` (a script file, to which stdin is data) or ``bash -c '...'``."""
    if tool_of(argv) not in SHELLS:
        return False
    words = iter(without_redirections(argv)[1:])
    for w in words:
        if re.fullmatch(r"-[A-Za-z]*c[A-Za-z]*", w):
            return False
        if re.fullmatch(r"-[A-Za-z]*s[A-Za-z]*", w):
            return True
        if w == "--":
            return next(words, None) is None
        if w in SHELL_VALUES:
            next(words, None)
        elif not w.startswith(("-", "+")):
            return False
    return True


def inner_scripts(argv: list[str]) -> list[str]:
    """The scripts a command runs from its own words: ``bash -c`` (``-lc``, ``-ec``), a here-string given to a shell
    (``bash <<< '...'``), ``pwsh -c``/``-Command``, ``cmd /c`` and ``eval``."""
    tool = tool_of(argv)
    if tool in SHELLS:
        scripts = []
        if "<<<" in argv:
            at = argv.index("<<<")
            if at + 1 < len(argv) and shell_reads_stdin(argv[:at] + argv[at + 2:]):
                scripts.append(argv[at + 1])
        # -c, or -c among bundled options: the next word is the script, the words after it its $0 and arguments
        at = next((i for i, w in enumerate(argv[1:], 1) if re.fullmatch(r"-[A-Za-z]*c[A-Za-z]*", w)), None)
        if at is not None and at + 1 < len(argv):
            scripts.append(argv[at + 1])
        return scripts
    if tool in ("pwsh", "powershell"):
        # -c, -Command and every prefix of it: the words after it, joined, are the script
        at = next((i for i, w in enumerate(argv[1:], 1)
                   if re.fullmatch(r"[-/]c(?:o(?:m(?:m(?:a(?:n(?:d)?)?)?)?)?)?", w, re.IGNORECASE)), None)
        return [" ".join(argv[at + 1:])] if at is not None and at + 1 < len(argv) else []
    if tool == "cmd":
        at = next((i for i, w in enumerate(argv[1:], 1) if w.lower() in ("/c", "/k")), None)
        return [" ".join(argv[at + 1:])] if at is not None and at + 1 < len(argv) else []
    if tool == "eval" and len(argv) >= 2:
        return [" ".join(argv[1:])]
    return []


def shell_commands(text: str) -> list[list[str]]:
    """The simple commands of a script, each its words. Read as commands too: the scripts of :func:`inner_scripts`,
    and a heredoc's body when a shell reads it as its script (``bash <<EOF``, ``bash -s <<'EOF'``, ``cat <<EOF |
    bash``). Other heredocs (``cat > f <<EOF``, ``python3 - <<PY``) are data, and so is quoted text: a ``<<`` inside
    quotes or a comment starts no heredoc."""
    text = EXPR.sub("__EXPR__", text).replace("\\\r\n", " ").replace("\\\n", " ")
    commands: list[list[str]] = []
    ends: list[str] = []  # the operator that ended each command
    bodies: list[tuple[str, list[str]]] = []  # each heredoc's body, with its command's words (the same list)
    pending: list[tuple[str, list[str]]] = []  # the heredocs met on this line: their delimiter and command
    words, word, quote, started = [], [], None, False
    i = 0

    def end_word():
        nonlocal word, started
        if started:
            words.append("".join(word))
        word, started = [], False

    def end_command(op):
        nonlocal words
        end_word()
        if words:
            commands.append(words)
            ends.append(op)
        words = []

    while i < len(text):
        ch = text[i]
        if quote == "'":
            if ch == "'":
                quote = None
            else:
                word.append(ch)
        elif quote == '"':
            if ch == '"':
                quote = None
            elif ch == "\\" and i + 1 < len(text) and text[i + 1] in '"\\$`':
                word.append(text[i + 1])
                i += 1
            else:
                word.append(ch)
        elif ch in "'\"":
            quote, started = ch, True
        elif ch == "\\" and i + 1 < len(text):
            word.append(text[i + 1])
            started = True
            i += 1
        elif ch == "#" and not started:
            while i < len(text) and text[i] != "\n":
                i += 1
            continue
        elif text.startswith("<<<", i):
            end_word()
            words.append("<<<")
            i += 3
            continue
        elif text.startswith("<<", i):
            end_word()
            delimiter, i = heredoc_delimiter(text, i + 2)
            if delimiter:
                pending.append((delimiter, words))
            continue
        elif ch == "&" and (text.startswith("&>", i) or (word and word[-1] in "<>")):
            word.append(ch)  # a redirection (&>log, 2>&1), not a separator
            started = True
        elif ch == "\n":
            end_command("\n")
            for delimiter, owner in pending:  # the bodies follow this line, one after another
                body, i = heredoc_body(text, i + 1, delimiter)
                bodies.append((body, owner))
            pending = []
        elif ch in SEPARATORS:
            op = text[i:i + 2] if text[i:i + 2] in ("&&", "||", "|&", ";;") else ch
            end_command("|" if op == "|&" else op)
            i += len(op)
            continue
        elif ch in " \t\r":
            end_word()
        else:
            word.append(ch)
            started = True
        i += 1
    end_command("")
    nested = []
    for body, owner in bodies:
        at = next((n for n, c in enumerate(commands) if c is owner), None)
        # the heredoc's own command, or a command its output is piped into (cat <<EOF | bash)
        while at is not None:
            if shell_reads_stdin(command_words(commands[at])):
                nested += shell_commands(body)
                break
            at = at + 1 if ends[at] == "|" and at + 1 < len(commands) else None
    for words in commands:
        for script in inner_scripts(command_words(words)):
            nested += shell_commands(script)
    return commands + nested


def command_words(words: list[str]) -> list[str]:
    """A command's words from the command itself: leading assignments, keywords and wrappers dropped."""
    i = 0
    while i < len(words):
        w = words[i]
        if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", w, re.DOTALL) or w in KEYWORDS or w in WRAPPERS:
            i += 1
        elif w == "sudo":
            i += 1
            while i < len(words) and words[i].startswith("-"):
                option = words[i]
                i += 2 if option in SUDO_VALUES else 1
                if option == "--":
                    break
        elif w == "env":
            i += 1
            while i < len(words) and (words[i].startswith("-") or "=" in words[i]):
                i += 1
        elif w in ("nice", "timeout"):
            i += 1
            while i < len(words) and (words[i].startswith("-") or re.fullmatch(r"[\d.]+[smhd]?", words[i])):
                i += 1
        else:
            break
    return words[i:]


def tool_of(argv: list[str]) -> str:
    """The program a command runs, by its base name: ``python -m pip`` is pip, ``py -3 -m build`` is build."""
    if not argv:
        return ""
    name = re.split(r"[\\/]", argv[0])[-1].lower()
    name = name[:-4] if name.endswith(".exe") else name
    if re.fullmatch(r"pip-?[0-9.]*", name):  # pip3, pip3.12: the runners ship each
        return "pip"
    if re.fullmatch(r"python[0-9.]*|py|pypy[0-9.]*", name):
        # the interpreter's own options come before -m; a script's arguments after its path are its own
        words = iter(argv[1:])
        for w in words:
            if w == "-m":
                return next(words, name).lower()
            if w in ("-X", "-W", "-Q"):
                next(words, None)
            elif not w.startswith("-"):
                break
    return name


def subcommand(argv: list[str], takes_value: tuple[str, ...]) -> list[str]:
    """A command's words after its global options: ``git -C dir push`` is ``push``, ``gh -R o/r release create`` is
    ``release create``. ``takes_value`` names the options whose next word is their value."""
    rest = argv[1:]
    while rest and rest[0].startswith("-"):
        rest = rest[2:] if rest[0] in takes_value else rest[1:]
    return rest


def gh_api_mutates(argv: list[str]) -> bool:
    method = None
    for i, w in enumerate(argv):
        if w in ("-X", "--method") and i + 1 < len(argv):
            method = argv[i + 1].upper()
        elif w.startswith("--method="):
            method = w.split("=", 1)[1].upper()
        elif re.fullmatch(r"-X[A-Za-z]+", w):
            method = w[2:].upper()
    if method is None:
        fields = any(w in ("-f", "-F", "--field", "--raw-field", "--input") or w.startswith(("--field=",
                     "--raw-field=", "--input=")) for w in argv)
        method = "POST" if fields else "GET"
    return method not in ("GET", "HEAD")


GH_WRITES = {
    "release": {"create", "upload", "edit", "delete", "delete-asset"},
    "workflow": {"run", "enable", "disable"},
    "pr": {"create", "merge", "comment", "edit", "close", "reopen", "review", "ready", "lock", "unlock"},
    "issue": {"create", "comment", "edit", "close", "reopen", "delete", "lock", "unlock", "transfer", "pin", "unpin"},
    "secret": {"set", "delete", "remove"},
    "variable": {"set", "delete"},
    "label": {"create", "edit", "delete", "clone"},
    "repo": {"create", "edit", "delete", "rename", "archive", "unarchive", "sync", "fork", "deploy-key"},
    "run": {"cancel", "rerun", "delete"},
    "cache": {"delete"},
}


def publishing_command(argv: list[str]) -> str | None:
    tool = tool_of(argv)
    rest = argv[1:]
    if tool == "gh":
        rest = subcommand(argv, ("-R", "--repo", "--hostname"))
        if rest[:1] == ["api"]:
            return "gh api (a mutating request)" if gh_api_mutates(rest) else None
        if len(rest) >= 2 and rest[1] in GH_WRITES.get(rest[0], ()):
            return f"gh {rest[0]} {rest[1]}"
        return None
    if tool == "git" and subcommand(argv, ("-C", "-c", "--git-dir", "--work-tree", "--namespace"))[:1] == ["push"]:
        return "git push"
    if tool == "twine" and "upload" in rest:
        return "twine upload"
    if tool in ("docker", "podman"):
        if "push" in rest[:2] or ("manifest" in rest[:2] and "push" in rest):
            return f"{tool} push"
        if "imagetools" in rest and "create" in rest:
            return "imagetools create"
        if "build" in rest[:2] and "--push" in rest:
            return f"{tool} build --push"
    if tool in ("npm", "cargo", "uv", "poetry") and rest[:1] == ["publish"]:
        return f"{tool} publish"
    if tool == "maturin" and rest[:1] in (["publish"], ["upload"]):
        return f"maturin {rest[0]}"
    return None


def build_command(argv: list[str]) -> str | None:
    tool = tool_of(argv)
    rest = argv[1:]
    if tool in ("cargo", "maturin", "pyinstaller", "iscc", "build"):
        return tool
    if tool == "pip" and "wheel" in rest[:1]:
        return "pip wheel"
    if tool in ("docker", "podman") and ("build" in rest[:1] or rest[:2] == ["buildx", "build"]):
        return f"{tool} build"
    return None


def wiki_push(argv: list[str]) -> bool:
    return any(re.split(r"[\\/]", w)[-1] == "build_wiki.py" for w in argv) and "--push" in argv


# ================================================================================================ workflows


def action_of(uses) -> str:
    """``owner/repo`` (lowercased) of a remote ``uses:``; the path for a local one."""
    if not isinstance(uses, str):
        return ""
    ref = uses.split("@", 1)[0]
    if ref.startswith("./") or ref.startswith("docker://"):
        return ref
    return "/".join(ref.split("/")[:2]).lower()


def is_setup_action(action: str) -> bool:
    return action in SETUP_ACTIONS or re.fullmatch(r"(actions|docker)/setup-[a-z0-9-]+", action) is not None


def strings(value):
    """Every string inside a YAML value."""
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for k, v in value.items():
            if isinstance(k, str):
                yield k
            yield from strings(v)
    elif isinstance(value, list):
        for v in value:
            yield from strings(v)


SECRET_REF = re.compile(r"\bsecrets\b\s*(?:\.\s*([A-Za-z_][A-Za-z0-9_-]*)|\[\s*'([^']*)'\s*\])?", re.IGNORECASE)
#: ``github.token`` (or ``github['token']``): the same token as ``secrets.GITHUB_TOKEN``.
GITHUB_TOKEN_REF = re.compile(r"\bgithub\s*(?:\.\s*token\b|\[\s*'token'\s*\])", re.IGNORECASE)


def secrets_read(value) -> set[str]:
    """The secrets a value's expressions read, by name (``*`` for the whole context; ``github.token`` is
    ``GITHUB_TOKEN``)."""
    out = set()
    for s in strings(value):
        for expr in EXPR.findall(s):
            for match in SECRET_REF.finditer(expr):
                out.add((match[1] or match[2] or "*").upper())
            if GITHUB_TOKEN_REF.search(expr):
                out.add("GITHUB_TOKEN")
    return out


def other_secrets(names: set[str]) -> set[str]:
    return {n for n in names if n != "GITHUB_TOKEN"}


def write_permissions(perms) -> list[str]:
    if isinstance(perms, str):
        return [perms] if perms.strip() == "write-all" else []
    if isinstance(perms, dict):
        return sorted(f"{k}: {v}" for k, v in perms.items() if str(v).strip() == "write")
    return []


def defaults_shell(holder) -> object:
    defaults = holder.get("defaults") if isinstance(holder, dict) else None
    run = defaults.get("run") if isinstance(defaults, dict) else None
    return run.get("shell") if isinstance(run, dict) else None


def unreadable_shell(wf: Workflow, job: dict, step: dict) -> str | None:
    """The shell of a run step whose commands this check cannot read (``python``, ``perl {0}``, an expression), or
    None. The step's ``shell:`` counts, else the job's or the workflow's ``defaults.run.shell``, else the runner's
    default (bash, or pwsh on Windows); a custom template (``bash -e {0}``) by its program."""
    if not isinstance(step.get("run"), str):
        return None
    for value in (step.get("shell"), defaults_shell(job), defaults_shell(wf.data)):
        text = str(value).strip() if value is not None else ""
        if not text:
            continue
        if "${{" in text:
            return text
        name = re.split(r"[\\/]", text.split()[0])[-1].lower()
        name = name[:-4] if name.endswith(".exe") else name
        return None if name in READABLE_SHELLS else name
    return None


def environment_name(env) -> str | None:
    if env is None:
        return None
    if isinstance(env, dict):
        env = env.get("name")
    return str(env) if env is not None else None


def triggers(data: dict) -> dict:
    on = data.get("on", data.get(True))
    if isinstance(on, str):
        return {on: None}
    if isinstance(on, list):
        return {str(k): None for k in on}
    return on if isinstance(on, dict) else {}


def concurrency_group(value) -> str | None:
    if isinstance(value, dict):
        value = value.get("group")
    return str(value) if value is not None else None


def runner_os(runs_on) -> str | None:
    labels = runs_on if isinstance(runs_on, list) else [runs_on]
    for label in labels:
        label = str(label).lower()
        for prefix, os_name in (("ubuntu", "Linux"), ("windows", "Windows"), ("macos", "macOS")):
            if label.startswith(prefix):
                return os_name
    return None


def matrix_legs(job: dict) -> list[dict]:
    """Each leg's matrix values, as GitHub expands ``strategy.matrix`` (its product, ``exclude``, then ``include``)."""
    matrix = (job.get("strategy") or {}).get("matrix")
    if not isinstance(matrix, dict):
        return [{}]
    base = {k: v for k, v in matrix.items() if k not in ("include", "exclude")}
    keys = list(base)
    values = [v if isinstance(v, list) else [v] for v in base.values()]
    legs = [dict(zip(keys, combo, strict=True)) for combo in itertools.product(*values)] if keys else []
    for ex in matrix.get("exclude") or []:
        if isinstance(ex, dict):
            legs = [leg for leg in legs if not all(leg.get(k) == v for k, v in ex.items())]
    originals = [dict(leg) for leg in legs]
    for inc in matrix.get("include") or []:
        if not isinstance(inc, dict):
            continue
        added = False
        for leg, original in zip(legs[:len(originals)], originals, strict=True):
            if all(original.get(k, v) == v for k, v in inc.items() if k in original):
                leg.update(inc)
                added = True
        if not added:
            legs.append(dict(inc))
    return legs or [{}]


def expand(text, leg: dict, os_name: str | None, index: int) -> str:
    """A name with each leg's ``matrix.*``, ``runner.os`` and ``strategy.job-index`` filled in; other expressions
    stay as written."""
    def fill(match):
        inner = match[1].strip()
        found = re.fullmatch(r"matrix\.([A-Za-z0-9_-]+)", inner)
        if found and found[1] in leg:
            value = leg[found[1]]
            return str(value).lower() if isinstance(value, bool) else str(value)
        if inner == "runner.os" and os_name:
            return os_name
        if inner == "strategy.job-index":
            return str(index)
        return match[0]
    return EXPR.sub(fill, str(text))


@dataclass
class Workflow:
    name: str  # the file's name, e.g. release.yml
    path: Path
    text: str
    data: dict

    @property
    def jobs(self) -> dict:
        jobs = self.data.get("jobs")
        return jobs if isinstance(jobs, dict) else {}

    def steps(self, job: dict) -> list[dict]:
        steps = job.get("steps")
        return [s for s in steps if isinstance(s, dict)] if isinstance(steps, list) else []

    def job_at_lines(self) -> dict[int, str]:
        """Each line of the jobs block (1-based) and the job it belongs to, read from the text."""
        out, in_jobs, indent, current = {}, False, None, None
        for number, line in enumerate(self.text.splitlines(), 1):
            body = line.lstrip()
            if not body or body.startswith("#"):
                if current:
                    out[number] = current
                continue
            depth = len(line) - len(body)
            if depth == 0:
                in_jobs = body.split("#")[0].strip() == "jobs:"
                indent, current = None, None
                continue
            if in_jobs:
                indent = depth if indent is None else indent
                if depth == indent:
                    current = body.split(":")[0].strip().strip("'\"")
                if current:
                    out[number] = current
        return out


def load(root: Path) -> tuple[list[Workflow], list[Finding]]:
    folder = root / WORKFLOWS
    found, problems = [], []
    for path in sorted([*folder.glob("*.yml"), *folder.glob("*.yaml")]):
        text = path.read_text(encoding="utf-8")
        try:
            data = yaml.safe_load(text)
        except yaml.YAMLError as e:
            problems.append(Finding(path.name, None, 1, f"does not parse: {str(e).splitlines()[0]}"))
            continue
        if not isinstance(data, dict) or not isinstance(data.get("jobs"), dict):
            problems.append(Finding(path.name, None, 1, "is not a workflow (no jobs mapping)"))
            continue
        found.append(Workflow(path.name, path, text, data))
    return found, problems


# ================================================================================================ the rules


def job_reasons(wf: Workflow, job: dict) -> tuple[list[str], list[tuple[int, dict, list[str]]]]:
    """Why a job publishes: (job-level reasons, [(step index, step, the step's reasons)])."""
    level = []
    env = environment_name(job.get("environment"))
    if env:
        level.append(f"names the environment {env!r}")
    writes = write_permissions(job.get("permissions"))
    if writes:
        level.append(f"asks for {', '.join(writes)}")
    secret_names = other_secrets(secrets_read({k: v for k, v in job.items() if k != "steps"})
                                 | secrets_read(wf.data.get("env")))
    if job.get("secrets") == "inherit":
        secret_names.add("*")
    if secret_names:
        level.append(f"reads the secret(s) {', '.join(sorted(secret_names))} for every step")
    steps = []
    for i, step in enumerate(wf.steps(job)):
        reasons = []
        names = other_secrets(secrets_read(step))
        if names:
            reasons.append(f"reads the secret(s) {', '.join(sorted(names))}")
        action = action_of(step.get("uses"))
        if action in PUBLISHING_ACTIONS:
            reasons.append(f"uses {action}")
        if action == "docker/build-push-action":
            push = (step.get("with") or {}).get("push", False)
            if push not in (False, "false", None):
                reasons.append("uses docker/build-push-action with push")
        if isinstance(step.get("run"), str):
            for argv in shell_commands(step["run"]):
                what = publishing_command(command_words(argv))
                if what:
                    reasons.append(f"runs {what}")
        if reasons:
            steps.append((i, step, reasons))
    return level, steps


def step_label(i: int, step: dict) -> str:
    name = step.get("name") or action_of(step.get("uses")) or (step.get("run") or "").strip().split("\n")[0][:60]
    return f"step {i + 1} ({name})"


def rule_1(wf: Workflow) -> list[Finding]:
    perms = wf.data.get("permissions")
    if perms != {"contents": "read"}:
        return [Finding(wf.name, None, 1, f"sets permissions {perms!r} at the top, not contents: read")]
    return []


def rule_2_3(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        level, steps = job_reasons(wf, job)
        if not level and not steps:
            continue
        everything = level + [r for _, _, rs in steps for r in rs]
        if wf.name in (TESTS, PACKAGE):
            out.append(Finding(wf.name, job_id, 3, f"publishes ({'; '.join(everything)}): {wf.name} publishes "
                                                   f"nothing"))
            out.append(Finding(wf.name, job_id, 2, f"publishes ({'; '.join(everything)}) outside release.yml and "
                                                   f"docs.yml"))
        elif wf.name == RELEASE:
            problem = guard_problem(job.get("if"), "tag")
            if problem:
                out.append(Finding(wf.name, job_id, 2, f"publishes ({'; '.join(everything)}) but {problem}"))
        elif wf.name == DOCS:
            if guard_problem(job.get("if"), "main") is None:
                continue
            if level:
                out.append(Finding(wf.name, job_id, 2, f"publishes ({'; '.join(level)}) but "
                                                       f"{guard_problem(job.get('if'), 'main')}"))
            for i, step, reasons in steps:
                problem = guard_problem(step.get("if"), "main")
                if problem:
                    out.append(Finding(wf.name, job_id, 2, f"{step_label(i, step)} publishes ({'; '.join(reasons)}) "
                                                           f"but {problem}"))
        else:
            out.append(Finding(wf.name, job_id, 2, f"publishes ({'; '.join(everything)}): only release.yml (on a "
                                                   f"tag push) and docs.yml (from main) may"))
    if wf.name in (TESTS, PACKAGE):
        names = secrets_read(wf.data)
        if names:
            out.append(Finding(wf.name, None, 3, f"names the secret(s) {', '.join(sorted(names))}: {wf.name} names "
                                                 f"no secret"))
        call = triggers(wf.data).get("workflow_call")
        if isinstance(call, dict) and call.get("secrets"):
            out.append(Finding(wf.name, None, 3, f"declares workflow_call secrets: {wf.name} names no secret"))
        for job_id, job in wf.jobs.items():
            if isinstance(job, dict) and environment_name(job.get("environment")):
                out.append(Finding(wf.name, job_id, 3, f"names the environment "
                                                       f"{environment_name(job.get('environment'))!r}"))
    return out


def rule_4(workflows: list[Workflow]) -> list[Finding]:
    out = []
    by_name = {wf.name: wf for wf in workflows}
    for wf in workflows:
        if "workflow_call" not in triggers(wf.data):
            continue
        groups = [(None, concurrency_group(wf.data.get("concurrency")))]
        groups += [(job_id, concurrency_group(job.get("concurrency"))) for job_id, job in wf.jobs.items()
                   if isinstance(job, dict)]
        for job_id, group in groups:
            if group is not None and not re.search(r"\$\{\{[^}]*\bgithub\.workflow\b[^}]*\}\}", group):
                out.append(Finding(wf.name, job_id, 4, f"is called by another workflow, and its concurrency group "
                                                       f"{group!r} lacks github.workflow, so a release run and a "
                                                       f"push's run would share it"))
        own = concurrency_group(wf.data.get("concurrency"))
        for caller in by_name.values():
            for job_id, job in caller.jobs.items():
                if not isinstance(job, dict) or not isinstance(job.get("uses"), str):
                    continue
                if job["uses"].split("@")[0] not in (f"./.github/workflows/{wf.name}",):
                    continue
                theirs = concurrency_group(caller.data.get("concurrency"))
                if own is not None and theirs is not None:
                    def as_run(group, caller=caller):
                        return re.sub(r"\$\{\{\s*github\.workflow\s*\}\}", str(caller.data.get("name", caller.name)),
                                      group)
                    if as_run(own) == as_run(theirs):
                        out.append(Finding(caller.name, job_id, 4, f"calls {wf.name}, whose concurrency group is "
                                                                   f"its own: the call would wait on itself"))
    return out


def rule_5(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        for i, step in enumerate(wf.steps(job)):
            run = step.get("run")
            if not isinstance(run, str):
                continue
            for expr in EXPR.findall(run):
                if TAINTED.search(expr):
                    out.append(Finding(wf.name, job_id, 5, f"{step_label(i, step)} writes ${{{{{expr}}}}} into its "
                                                           f"script; pass it through env:"))
    return out


def rule_6(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        defined = any("MATURIN_VERSION" in (env if isinstance(env, dict) else {})
                      for env in (wf.data.get("env"), job.get("env")))
        for i, step in enumerate(wf.steps(job)):
            if action_of(step.get("uses")) != "pyo3/maturin-action":
                continue
            pinned = str((step.get("with") or {}).get("maturin-version", ""))
            if re.sub(r"\s+", "", pinned) != "${{env.MATURIN_VERSION}}":
                out.append(Finding(wf.name, job_id, 6, f"{step_label(i, step)} does not pin maturin-version: "
                                                       f"${{{{ env.MATURIN_VERSION }}}} (it says {pinned!r})"))
            elif not defined:
                out.append(Finding(wf.name, job_id, 6, f"{step_label(i, step)} pins env.MATURIN_VERSION, which "
                                                       f"the workflow's env does not define"))
    return out


def is_prepare(step: dict) -> bool:
    """A run step whose script runs ``scripts/release.py prepare`` (``python``, ``python3``, ``py``, with options)."""
    if not isinstance(step.get("run"), str):
        return False
    for argv in (command_words(c) for c in shell_commands(step["run"])):
        if not argv or not re.fullmatch(r"python[0-9.]*|py", tool_of(argv[:1])):
            continue
        for j, w in enumerate(argv[1:-1], 1):
            if re.split(r"[\\/]", w)[-1] == "release.py" and argv[j + 1] == "prepare":
                return True
    return False


def rule_7(wf: Workflow) -> list[Finding]:
    if wf.name not in (TESTS, PACKAGE):
        return []
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict) or "uses" in job:
            continue
        steps = wf.steps(job)
        checkout = next((i for i, s in enumerate(steps) if action_of(s.get("uses")) == "actions/checkout"), None)
        if checkout is None:
            out.append(Finding(wf.name, job_id, 7, "has no checkout, so it cannot run scripts/release.py prepare"))
            continue
        first = next((i for i in range(checkout + 1, len(steps))
                      if not is_setup_action(action_of(steps[i].get("uses")))), None)
        if first is None or not is_prepare(steps[first]):
            prepared = next((i for i, s in enumerate(steps) if is_prepare(s)), None)
            if prepared is None:
                out.append(Finding(wf.name, job_id, 7, "runs no scripts/release.py prepare"))
            else:
                out.append(Finding(wf.name, job_id, 7, f"{step_label(first, steps[first])} runs before "
                                                       f"scripts/release.py prepare ({step_label(prepared, steps[prepared])}), which must come "
                                                       f"first after the checkout"))
            continue
        step = steps[first]
        if "if" in step or step.get("continue-on-error") not in (None, False):
            out.append(Finding(wf.name, job_id, 7, f"{step_label(first, step)} (release.py prepare) is conditional "
                                                   f"or may fail without failing the job"))
    return out


def rule_8(wf: Workflow) -> list[Finding]:
    if wf.name != RELEASE:
        return []
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        for i, step in enumerate(wf.steps(job)):
            action = action_of(step.get("uses"))
            if action in ("pyo3/maturin-action", "docker/build-push-action"):
                out.append(Finding(wf.name, job_id, 8, f"{step_label(i, step)} builds ({action}): release.yml "
                                                       f"builds nothing, package.yml does"))
            shell = unreadable_shell(wf, job, step)
            if shell is not None:
                out.append(Finding(wf.name, job_id, 8, f"{step_label(i, step)} runs in the shell {shell!r}, whose "
                                                       f"commands this check cannot read: release.yml's run steps "
                                                       f"use {', '.join(READABLE_SHELLS)}"))
            if isinstance(step.get("run"), str):
                for argv in shell_commands(step["run"]):
                    what = build_command(command_words(argv))
                    if what:
                        out.append(Finding(wf.name, job_id, 8, f"{step_label(i, step)} runs {what}: release.yml "
                                                               f"builds nothing, package.yml does"))
    return out


def upload_steps(wf: Workflow):
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        for i, step in enumerate(wf.steps(job)):
            action = action_of(step.get("uses"))
            if action in ("actions/upload-artifact", "actions/upload-pages-artifact"):
                yield job_id, job, i, step, action


TEMP = re.compile(r"\$\{\{\s*(?:runner\.temp|env\.runner_temp)\s*\}\}|\$\{?RUNNER_TEMP\}?", re.IGNORECASE)


def path_holds_key(path: str) -> str | None:
    p = path.strip().strip("'\"").replace("\\", "/")
    if not p or p.startswith("!"):
        return None
    parts = [x for x in p.split("/") if x]
    if any("throwaway" in part.lower() for part in parts):
        return "the throwaway key folder"
    if ".." in parts:
        return "a path that leaves the workspace (..)"
    if p in ("/", "~", "~/") or p.startswith("/home/runner/work/_temp"):
        return "the runner's own folders"
    temp = TEMP.search(p)
    if temp and not [x for x in p[temp.end():].split("/") if x and x not in ("*", "**")]:
        return "the runner's temporary folder, where keys are made"
    last = parts[-1] if parts else p
    if fnmatch.fnmatchcase("update-signing.key", last) or last.lower().endswith(".key"):
        return f"files named like a key ({last})"
    return None


def rule_9(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, _job, i, step, _action in upload_steps(wf):
        paths = str((step.get("with") or {}).get("path", ""))
        for line in paths.splitlines():
            why = path_holds_key(line)
            if why:
                out.append(Finding(wf.name, job_id, 9, f"{step_label(i, step)} uploads {line.strip()!r}, which can "
                                                       f"include {why}"))
    return out


def rule_10(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        uses = job.get("uses")
        if wf.name == RELEASE and isinstance(uses, str):
            called = uses.split("@")[0].rsplit("/", 1)[-1]
            if called in ("nightly.yml", "docs.yml", "nightly.yaml", "docs.yaml"):
                out.append(Finding(wf.name, job_id, 10, f"calls {called}: a release never runs the nightly runs or "
                                                        f"publishes the docs"))
        for i, step in enumerate(wf.steps(job)):
            if not isinstance(step.get("run"), str):
                continue
            for argv in shell_commands(step["run"]):
                argv = command_words(argv)
                if wiki_push(argv):
                    out.append(Finding(wf.name, job_id, 10, f"{step_label(i, step)} runs build_wiki.py --push"))
                if (wf.name == RELEASE and tool_of(argv) == "gh" and argv[1:3] == ["workflow", "run"]
                        and any(re.match(r"(nightly|docs)(\.ya?ml)?$", w) for w in argv[3:])):
                    out.append(Finding(wf.name, job_id, 10, f"{step_label(i, step)} dispatches nightly.yml or "
                                                            f"docs.yml"))
    return out


def rule_11(wf: Workflow) -> list[Finding]:
    out = []
    jobs_by_line = wf.job_at_lines()
    seen: dict[str, int] = {}
    for number, line in enumerate(wf.text.splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        match = USES_LINE.match(line)
        if not match:
            continue
        ref = match["ref"]
        seen[ref] = seen.get(ref, 0) + 1
        if ref.startswith("./"):
            continue
        job = jobs_by_line.get(number)
        if ref.startswith("docker://"):
            if not re.search(r"@sha256:[0-9a-f]{64}$", ref):
                out.append(Finding(wf.name, job, 11, f"line {number}: {ref} is not pinned by a sha256 digest"))
            continue
        target = ref.split("@", 1)[1] if "@" in ref else ""
        comment = (match["comment"] or "").split()
        if not SHA40.fullmatch(target):
            out.append(Finding(wf.name, job, 11, f"line {number}: {ref} is not pinned to a 40-character commit SHA "
                                                 f"(scripts/ci/action_pins.py pin)"))
        elif not comment or not VERSION_COMMENT.fullmatch(comment[0]):
            out.append(Finding(wf.name, job, 11, f"line {number}: {ref} has no version in a comment on its line "
                                                 f"(`@<sha> # v7.0.1`)"))
    parsed: dict[str, list[str]] = {}
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        refs = [job["uses"]] if isinstance(job.get("uses"), str) else []
        refs += [s["uses"] for s in wf.steps(job) if isinstance(s.get("uses"), str)]
        for ref in refs:
            parsed.setdefault(ref, []).append(job_id)
    for ref, jobs in parsed.items():
        if not ref.startswith("./") and len(jobs) > seen.get(ref, 0):
            out.append(Finding(wf.name, jobs[0], 11, f"{ref} is written where its pin and comment cannot be read "
                                                     f"(put `uses:` on a line of its own)"))
    return out + image_findings(wf)


def cli_value(args: str, flag: str) -> str:
    """The value of ``flag`` in an action's ``args`` string (``--target X`` or ``--target=X``), else ``""``."""
    words = args.split()
    for i, word in enumerate(words):
        if word == flag and i + 1 < len(words):
            return words[i + 1]
        if word.startswith(flag + "="):
            return word[len(flag) + 1:]
    return ""


def maturin_image_problem(given: dict, value, os_name: str | None) -> str | None:
    """Why a maturin-action step would build in an image not pinned by digest, or None. As the action decides
    (PyO3/maturin-action's ``innerMain``): only on Linux and only for ``build`` and ``publish`` does it build in a
    container; the one ``container:`` names (``off``: none), or else, when ``manylinux`` asks for one, one from its
    table by a moving tag. ``manylinux`` defaults to ``auto`` for ``publish`` and for a Linux target it cross-compiles
    to (read here whatever the runner's architecture, which is the stricter reading)."""
    if os_name not in (None, "Linux"):
        return None
    command = value(given.get("command")) or "build"
    if command not in ("build", "publish"):
        return None
    container = value(given.get("container"))
    if container == "off":
        return None
    if container and container.lower() not in MATURIN_TABLE_IMAGE:
        return None if DIGEST.search(container) else f"its container {container} is not pinned by a sha256 digest"
    manylinux = re.sub(r"^manylinux_?", "", value(given.get("manylinux")))
    target = value(given.get("target")) or cli_value(value(given.get("args")), "--target")
    cross = "linux" in target and "android" not in target and not ("x86_64" in target or "i686" in target)
    if not manylinux and (command == "publish" or cross):
        manylinux = "auto"
    if container or (manylinux and manylinux != "off"):
        return (f"builds in maturin-action's own image for manylinux {manylinux or 'auto'}, a moving tag: name its "
                f"container by a sha256 digest, or set manylinux: off")
    return None


#: A key whose value names an image a script runs (``docker run "$IMAGE"`` takes it from there, since a script's
#: words name no image reliably): a matrix leg's ``container`` or ``image``, or a matrix or ``env:`` key ending in
#: ``image`` or ``container`` (``FEDORA_IMAGE``), added in 5-03 for the helpers' container builds.
IMAGE_KEY = re.compile(r"(?:^|[_-])(?:container|image)$", re.IGNORECASE)


def named_images(wf: Workflow, job: dict) -> list[tuple[str, str]]:
    """The images a job's matrix and ``env:`` blocks (the workflow's, the job's, each step's) name by key, each
    with where it is written; an expression or an empty value names none here (its source is read where it is
    written)."""
    out = []
    for index, leg in enumerate(matrix_legs(job)):
        for key, value in leg.items():
            if IMAGE_KEY.search(str(key)) and isinstance(value, str):
                out.append((f"its matrix's {key} (leg {leg.get('platform') or leg.get('os') or index})", value))
    blocks = [("the workflow's env", wf.data.get("env")), ("its env", job.get("env"))]
    blocks += [(f"{step_label(i, step)}'s env", step.get("env")) for i, step in enumerate(wf.steps(job))]
    for where, env in blocks:
        for key, value in (env.items() if isinstance(env, dict) else ()):
            if IMAGE_KEY.search(str(key)) and isinstance(value, str):
                out.append((f"{where} {key}", value))
    # maturin-action's switches (``off``, ``auto``, ...) name no image: its steps are read above, as it decides
    switches = (*MATURIN_TABLE_IMAGE, "off")
    return [(where, value.strip()) for where, value in out
            if value.strip() and "${{" not in value and value.strip().lower() not in switches]


def image_findings(wf: Workflow) -> list[Finding]:
    """Rule 11's images, each matrix leg's: a job's ``container:`` and ``services:``, the image each maturin-action
    step builds in, and an image named by key for a script to run, each pinned by a sha256 digest."""
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        for where, image in named_images(wf, job):
            if not DIGEST.search(image):
                out.append(Finding(wf.name, job_id, 11, f"{where} names the image {image}, not pinned by a sha256 "
                                                        f"digest"))
        legs = matrix_legs(job)
        runs_on = job.get("runs-on", "")
        labels = runs_on if isinstance(runs_on, list) else [runs_on]
        for index, leg in enumerate(legs):
            os_name = runner_os([expand(label, leg, None, index) for label in labels])

            def value(raw, leg=leg, os_name=os_name, index=index) -> str:
                if raw is None:
                    return ""
                # A key a leg does not have reads as empty, as GitHub reads it.
                text = expand(raw, leg, os_name, index)
                return re.sub(r"\$\{\{\s*matrix\.[A-Za-z0-9_-]+\s*\}\}", "", text).strip()

            where = ""
            if len(legs) > 1:
                where = f" (leg {leg.get('platform') or leg.get('os') or leg.get('runner') or index})"
            images = []
            container = job.get("container")
            if isinstance(container, dict):
                container = container.get("image")
            if container is not None:
                images.append(("its container", value(container)))
            services = job.get("services")
            for name, service in (services.items() if isinstance(services, dict) else ()):
                image = service.get("image") if isinstance(service, dict) else service
                if image is not None:
                    images.append((f"its service {name}", value(image)))
            for what, image in images:
                if image and not DIGEST.search(image):
                    out.append(Finding(wf.name, job_id, 11, f"{what} {image}{where} is not pinned by a sha256 "
                                                            f"digest"))
            for i, step in enumerate(wf.steps(job)):
                if action_of(step.get("uses")) != "pyo3/maturin-action":
                    continue
                given = step.get("with")
                problem = maturin_image_problem(given if isinstance(given, dict) else {}, value, os_name)
                if problem:
                    out.append(Finding(wf.name, job_id, 11, f"{step_label(i, step)}{where}: {problem}"))
    return out


def rule_12(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict) or not isinstance(job.get("uses"), str):
            continue
        if "secrets" in job:
            what = "secrets: inherit" if job["secrets"] == "inherit" else "secrets"
            out.append(Finding(wf.name, job_id, 12, f"calls {job['uses']} with {what}: a called workflow gets none"))
        if job.get("permissions") != {"contents": "read"}:
            out.append(Finding(wf.name, job_id, 12, f"calls {job['uses']} with permissions "
                                                    f"{job.get('permissions')!r}, not contents: read"))
    return out


def rule_13(workflows: list[Workflow]) -> list[Finding]:
    out = []
    names: dict[str, list[tuple[str, str]]] = {}
    for wf in workflows:
        for job_id, job, i, step, action in upload_steps(wf):
            default = "github-pages" if action == "actions/upload-pages-artifact" else "artifact"
            template = (step.get("with") or {}).get("name", default)
            legs = matrix_legs(job)
            expanded = []
            runs_on = job.get("runs-on", "")
            for index, leg in enumerate(legs):
                labels = runs_on if isinstance(runs_on, list) else [runs_on]
                os_name = runner_os([expand(label, leg, None, index) for label in labels])
                expanded.append(expand(template, leg, os_name, index))
            own = PREFIXES.get(wf.name)
            for name in sorted(set(expanded)):
                if own and not name.startswith(own):
                    out.append(Finding(wf.name, job_id, 13, f"{step_label(i, step)} uploads {name!r}: {wf.name}'s "
                                                            f"artifacts are named {own}..."))
                for other, prefix in PREFIXES.items():
                    if other != wf.name and name.startswith(prefix):
                        out.append(Finding(wf.name, job_id, 13, f"{step_label(i, step)} uploads {name!r}, under "
                                                                f"{other}'s prefix {prefix}"))
            if wf.name in SHARED_NAMESPACE:
                for name in expanded:
                    names.setdefault(name, []).append((wf.name, job_id))
        if wf.name in (PACKAGE, RELEASE):
            for job_id, job in wf.jobs.items():
                if not isinstance(job, dict):
                    continue
                for i, step in enumerate(wf.steps(job)):
                    if action_of(step.get("uses")) != "actions/download-artifact":
                        continue
                    given = step.get("with") or {}
                    if given.get("name"):
                        continue
                    pattern = given.get("pattern")
                    if pattern is None:
                        out.append(Finding(wf.name, job_id, 13, f"{step_label(i, step)} downloads every artifact of "
                                                                f"the run: name one, or the pattern pkg-* or rel-*"))
                    elif str(pattern) not in DOWNLOAD_PATTERNS:
                        out.append(Finding(wf.name, job_id, 13, f"{step_label(i, step)} downloads the pattern "
                                                                f"{pattern!r}: only pkg-* or rel-*"))
    for name, places in sorted(names.items()):
        if len(places) > 1:
            where = ", ".join(f"{f} job {j}" for f, j in places)
            out.append(Finding(places[0][0], places[0][1], 13, f"the artifact name {name!r} is uploaded more than "
                                                               f"once in a release run's namespace ({where})"))
    return out


def rule_14(wf: Workflow) -> list[Finding]:
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict) or job.get("environment") is None:
            continue
        name = environment_name(job["environment"])
        if wf.name == RELEASE:
            if name == "pypi" and job_id == "publish-pypi":
                continue
            through_tag = tag_expression_name(name)
            if through_tag in TAG_ENVIRONMENTS:
                continue
            if through_tag is not None:
                out.append(Finding(wf.name, job_id, 14, f"names the environment {through_tag!r} through the tag "
                                                        f"expression: only {', '.join(TAG_ENVIRONMENTS)} is named "
                                                        f"that way, and any other name would make a new environment "
                                                        f"at the tag push"))
                continue
            out.append(Finding(wf.name, job_id, 14, f"names the environment {name!r}: release.yml names pypi (in "
                                                    f"publish-pypi) or the tag expression, which names nothing on a "
                                                    f"dispatch"))
        elif wf.name == DOCS:
            if name == "github-pages" and job_id == "deploy":
                continue
            out.append(Finding(wf.name, job_id, 14, f"names the environment {name!r}: docs.yml names github-pages "
                                                    f"(in deploy) only"))
        else:
            out.append(Finding(wf.name, job_id, 14, f"names the environment {name!r}: only release.yml and docs.yml "
                                                    f"name one"))
    return out


def rule_15(wf: Workflow) -> list[Finding]:
    out = []
    workflow_secrets = other_secrets(secrets_read(wf.data.get("env")))
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        job_secrets = workflow_secrets | other_secrets(secrets_read(job.get("env")))
        for i, step in enumerate(wf.steps(job)):
            names = job_secrets | other_secrets(secrets_read(step))
            if not names:
                continue
            if "uses" in step:
                out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)} reads {', '.join(sorted(names))} in "
                                                        f"an action's step: a secret reaches only a run step of its "
                                                        f"own"))
            shell = unreadable_shell(wf, job, step)
            if shell is not None:
                out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)} reads {', '.join(sorted(names))} in "
                                                        f"the shell {shell!r}, whose commands this check cannot "
                                                        f"read: use {', '.join(READABLE_SHELLS)}"))
            for argv in shell_commands(step.get("run") or ""):
                tool = tool_of(command_words(argv))
                if tool in PACKAGE_TOOLS:
                    out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)} reads "
                                                            f"{', '.join(sorted(names))} and runs {tool}"))
        if wf.name != RELEASE:
            continue
        power = []
        if other_secrets(secrets_read(job)) or workflow_secrets:
            power.append("reads a secret")
        if write_permissions(job.get("permissions")):
            power.append(f"holds {', '.join(write_permissions(job.get('permissions')))}")
        if not power:
            continue
        held = " and ".join(power)
        for i, step in enumerate(wf.steps(job)):
            action = action_of(step.get("uses"))
            if action:
                if action == "actions/checkout":
                    keep = (step.get("with") or {}).get("persist-credentials", True)
                    if keep not in (False, "false"):
                        out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)}: a job that {held} checks "
                                                                f"out with persist-credentials: false"))
                elif action not in POWER_ALLOWED and action not in PUBLISHING_ACTIONS:
                    out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)} uses {action} in a job that "
                                                            f"{held}: only the checkout, artifact and publishing "
                                                            f"actions may run there"))
            shell = unreadable_shell(wf, job, step)
            if shell is not None:
                out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)} runs in the shell {shell!r} in a "
                                                        f"job that {held}: its commands cannot be read, so it may "
                                                        f"install or build"))
            for argv in shell_commands(step.get("run") or ""):
                argv = command_words(argv)
                tool = tool_of(argv)
                built = build_command(argv)
                if tool in PACKAGE_TOOLS or (built and built.endswith(" build")):
                    out.append(Finding(wf.name, job_id, 15, f"{step_label(i, step)} runs {built or tool} in a job "
                                                            f"that {held}"))
    return out


@dataclass(frozen=True)
class ReleaseJob:
    """A row of P5.11's table: the jobs it needs directly (``meta`` may be added to any), when it runs (``tag``,
    ``dispatch`` or ``both``), the write permissions it holds, exactly, and the environment it names."""
    needs: frozenset
    when: str
    writes: frozenset = frozenset()
    environment: str | None = None
    calls: str | None = None


RELEASE_JOBS = {
    "meta": ReleaseJob(frozenset(), "both"),
    "tests": ReleaseJob(frozenset({"meta"}), "both", calls="test.yml"),
    "package": ReleaseJob(frozenset({"meta"}), "both", calls="package.yml"),
    "sign-helper-manifest": ReleaseJob(frozenset({"tests", "package"}), "tag", environment="helper-signing"),
    "check-signature": ReleaseJob(frozenset({"sign-helper-manifest"}), "tag"),
    "attest-files": ReleaseJob(frozenset({"check-signature"}), "tag", frozenset({"id-token", "attestations"})),
    "publish-pypi": ReleaseJob(frozenset({"attest-files"}), "tag", frozenset({"id-token"}), "pypi"),
    "publish-image": ReleaseJob(frozenset({"attest-files"}), "tag", frozenset({"packages"})),
    "attest-image": ReleaseJob(frozenset({"publish-image"}), "tag",
                               frozenset({"packages", "id-token", "attestations"})),
    "github-release": ReleaseJob(frozenset({"publish-pypi", "attest-image"}), "tag", frozenset({"contents"})),
    # publish-image besides: the digest it outputs is the one the moving tags point at
    "image-tags": ReleaseJob(frozenset({"github-release", "publish-image"}), "tag", frozenset({"packages"})),
    "dry-run-summary": ReleaseJob(frozenset({"tests", "package"}), "dispatch"),
}
#: The secret the signing job holds, and the only one release.yml reads (P5.4.2; WORKERS.md's setup names it).
SIGNING_SECRET = "CITAR_UPDATE_SIGNING_KEY"
#: The tags publish-image may push, exactly: the version (its job's VERSION, which is meta's), alone or with an
#: architecture. Any other expansion of VERSION (``${VERSION%.*}``, the series) or any other word is refused.
PUBLISH_TAG = re.compile(r"\$(VERSION|\{VERSION\})(-(\$arch|\$\{arch\}|amd64|arm64))?")
#: The repository every image release.yml pushes names: the workflow's IMAGE_NAME, which package.yml's image job tags.
IMAGE_NAME_REF = re.compile(r"\$(IMAGE_NAME|\{IMAGE_NAME\})")
#: What publish-image's VERSION must be: meta's output, as every other job of release.yml takes it.
META_VERSION = "${{needs.meta.outputs.version}}"
#: docker's, podman's and buildah's global options that take a value (``docker --config DIR push ...``).
ENGINE_GLOBALS = ("--config", "-c", "--context", "-H", "--host", "-l", "--log-level", "--tlscacert", "--tlscert",
                  "--tlskey", "--root", "--runroot", "--storage-driver", "--storage-opt", "--url", "--connection",
                  "--identity", "--cgroup-manager", "--conmon", "--events-backend", "--tmpdir", "--module",
                  "--registries-conf", "--userns-uid-map", "--userns-gid-map")
#: A push's own options that take a value: that value is not a reference.
PUSH_VALUES = ("--platform", "--authfile", "--cert-dir", "--creds", "--digestfile", "--format", "-f",
               "--compression-format", "--compression-level", "--sign-by", "--sign-by-sigstore",
               "--sign-by-sigstore-private-key", "--sign-passphrase-file", "--signature-policy", "--encryption-key",
               "--encrypt-layer", "--retry", "--retry-delay")
#: Tools that copy images to a registry by other means than the two rule 16 reads (``docker push`` and ``docker
#: buildx imagetools create``, and their podman and buildah forms): release.yml uses none of them.
OTHER_PUSHERS = ("skopeo", "crane", "gcrane", "oras", "regctl", "nerdctl", "ko", "jib")


def needs_of(job: dict) -> set[str]:
    needs = job.get("needs")
    if isinstance(needs, str):
        return {needs}
    return {str(n) for n in needs} if isinstance(needs, list) else set()


def run_commands(wf: Workflow, job: dict) -> list[tuple[int, list[str]]]:
    """Every command a job's run steps run, with its step's index (words as command_words gives them)."""
    out = []
    for i, step in enumerate(wf.steps(job)):
        if isinstance(step.get("run"), str):
            out += [(i, command_words(argv)) for argv in shell_commands(step["run"])]
    return out


def gh_release(argv: list[str]) -> list[str]:
    """``gh release``'s subcommand and its words (``create``, ``edit``, ...), or ``[]``."""
    if tool_of(argv) != "gh":
        return []
    rest = subcommand(argv, ("-R", "--repo", "--hostname"))
    return rest[1:] if rest[:1] == ["release"] and len(rest) > 1 else []


def flag_value(words: list[str], name: str) -> str | None:
    """``--name``'s value: ``--name=x`` as ``x``, a bare ``--name`` as ``true``; None when absent."""
    for w in words:
        if w == name:
            return "true"
        if w.startswith(name + "="):
            return w.split("=", 1)[1]
    return None


def image_refs(argv: list[str]) -> list[str]:
    """The image references a push names: ``push``, ``image push`` and ``manifest push`` of docker, podman or buildah
    (each word it names, podman's and buildah's destination included), or the ``--tag``/``-t`` targets of
    ``imagetools create``."""
    tool = tool_of(argv)
    if tool not in ("docker", "podman", "buildah"):
        return []
    rest = subcommand(argv, ENGINE_GLOBALS)
    if rest[:1] == ["push"] or rest[:2] in (["image", "push"], ["manifest", "push"]):
        words = rest[1:] if rest[0] == "push" else rest[2:]
        refs, skip = [], False
        for w in words:
            if skip:
                skip = False
            elif w in PUSH_VALUES:
                skip = True
            elif not w.startswith("-"):  # a repository named without a tag (--all-tags, or :latest) has no version
                refs.append(w)
        return refs
    if "imagetools" in rest and "create" in rest:
        out = []
        for i, w in enumerate(rest):
            if w in ("--tag", "-t") and i + 1 < len(rest):
                out.append(rest[i + 1])
            elif w.startswith("--tag="):
                out.append(w.split("=", 1)[1])
        return out
    return []


def rule_16(wf: Workflow) -> list[Finding]:
    if wf.name != RELEASE:
        return []
    out = []

    def find(job, message):
        out.append(Finding(wf.name, job, 16, message))

    group = concurrency_group(wf.data.get("concurrency"))
    cancel = wf.data.get("concurrency", {}).get("cancel-in-progress") if isinstance(wf.data.get("concurrency"),
                                                                                    dict) else None
    if group is None or re.sub(r"\s+", "", group) != "release-${{github.ref}}" or cancel not in (False, "false"):
        find(None, f"its concurrency is {wf.data.get('concurrency')!r}, not group release-${{{{ github.ref }}}} "
                   f"with cancel-in-progress: false (a release is never cut off mid-publish)")
    jobs = wf.jobs
    for job_id in sorted(set(RELEASE_JOBS) - set(jobs)):
        find(job_id, f"P5.11's job {job_id} is missing")
    for job_id in sorted(set(jobs) - set(RELEASE_JOBS)):
        find(job_id, f"is not one of P5.11's jobs ({', '.join(RELEASE_JOBS)}): a job outside the table is outside "
                     f"its order")
    for job_id, row in RELEASE_JOBS.items():
        job = jobs.get(job_id)
        if not isinstance(job, dict):
            continue
        needs = needs_of(job)
        if needs - {"meta"} != row.needs - {"meta"} or not row.needs <= needs:
            find(job_id, f"needs {sorted(needs) or 'nothing'}, not {sorted(row.needs)}"
                         f"{' (and meta, for its outputs)' if 'meta' not in row.needs else ''}: P5.11's order")
        when = job.get("if")
        if row.when == "tag":
            problem = guard_problem(when, "tag")
            if problem:
                find(job_id, f"is tag-only but {problem}")
        elif row.when == "dispatch":
            parsed = condition(when) if when is not None else None
            weak = parsed is not None and any(n[0] == "call" and n[1] in WEAKENERS for n in walk(parsed))
            if parsed is None or weak or not any(is_equal(p, "github.event_name", "workflow_dispatch")
                                                 for p in conjuncts(parsed)):
                find(job_id, f"is a dry run's alone, but its if: ({when!r}) lacks github.event_name == "
                             f"'workflow_dispatch' as a conjunct")
        elif when is not None:
            find(job_id, f"runs on a tag and on a dry run alike, so it takes no if: ({when!r})")
        writes = {w.split(":")[0].strip() for w in write_permissions(job.get("permissions"))}
        if isinstance(job.get("permissions"), str):
            writes.add(str(job["permissions"]))
        if writes != set(row.writes):
            find(job_id, f"holds {sorted(writes) or 'no'} write permission(s), not P5.11's "
                         f"{sorted(row.writes) or 'none'}")
        env = job.get("environment")
        named = tag_expression_name(environment_name(env)) if env is not None else None
        if row.environment == "helper-signing" and named != "helper-signing":
            find(job_id, f"names {environment_name(env)!r}: the signing job names helper-signing through the tag "
                         f"expression, so its secret waits for the environment's reviewer")
        elif row.environment == "pypi" and environment_name(env) != "pypi":
            find(job_id, f"names {environment_name(env)!r}, not pypi (PyPI's trusted publisher names it)")
        elif row.environment is None and env is not None:
            find(job_id, f"names the environment {environment_name(env)!r}; P5.11 gives it none")
        if row.calls and str(job.get("uses", "")).split("@")[0] != f"./.github/workflows/{row.calls}":
            find(job_id, f"calls {job.get('uses')!r}, not ./.github/workflows/{row.calls}")
    out += release_signing(wf) + release_publishing(wf) + release_attestations(wf)
    return out


def release_signing(wf: Workflow) -> list[Finding]:
    """The signing job reads its one secret in one run step, whose env holds it alone, running release_sign.py."""
    out = []
    job_id = "sign-helper-manifest"
    for other_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        names = other_secrets(secrets_read(job) | secrets_read(wf.data.get("env")))
        if names and (other_id != job_id or names != {SIGNING_SECRET}):
            out.append(Finding(wf.name, other_id, 16, f"reads {', '.join(sorted(names))}: release.yml reads one "
                                                      f"secret, {SIGNING_SECRET}, in {job_id} alone"))
    job = wf.jobs.get(job_id)
    if not isinstance(job, dict):
        return out
    if other_secrets(secrets_read({k: v for k, v in job.items() if k != "steps"})):
        out.append(Finding(wf.name, job_id, 16, "reads the secret at the job's level: every step would hold it"))
    holders = [(i, s) for i, s in enumerate(wf.steps(job)) if other_secrets(secrets_read(s))]
    if len(holders) != 1:
        out.append(Finding(wf.name, job_id, 16, f"reads {SIGNING_SECRET} in {len(holders)} steps, not one"))
    for i, step in holders:
        env = step.get("env") if isinstance(step.get("env"), dict) else {}
        if list(env) != [SIGNING_SECRET] or other_secrets(secrets_read({k: v for k, v in step.items() if k != "env"})):
            out.append(Finding(wf.name, job_id, 16, f"{step_label(i, step)}: the secret goes in the env: of its one "
                                                    f"step, alone, as {SIGNING_SECRET}"))
        signs = any(any(re.split(r"[\\/]", w)[-1] == "release_sign.py" for w in argv) and "sign" in argv
                    for argv in (command_words(c) for c in shell_commands(step.get("run") or "")))
        if not signs:
            out.append(Finding(wf.name, job_id, 16, f"{step_label(i, step)} holds the secret but does not run "
                                                    f"scripts/ci/release_sign.py sign"))
    return out


def release_publishing(wf: Workflow) -> list[Finding]:
    """github-release's draft before it is public; publish-image's tags; who pushes images."""
    out = []
    for job_id, job in wf.jobs.items():
        if not isinstance(job, dict):
            continue
        commands = run_commands(wf, job)
        creates = [(i, argv) for i, argv in commands if gh_release(argv)[:1] == ["create"]]
        writes = [(i, argv) for i, argv in commands if gh_release(argv)[:1] and gh_release(argv)[0] in
                  GH_WRITES["release"]]
        if job_id != "github-release":
            if writes:
                out.append(Finding(wf.name, job_id, 16, "runs gh release writes: only github-release makes the "
                                                        "release"))
        else:
            for i, argv in creates:
                if flag_value(argv, "--draft") in (None, "false"):
                    out.append(Finding(wf.name, job_id, 16, f"step {i + 1} runs gh release create without --draft: "
                                                            f"the release is a draft until it is whole"))
            public = [i for i, argv in commands if gh_release(argv)[:1] == ["edit"]
                      and flag_value(argv, "--draft") == "false"]
            if not creates or not public:
                out.append(Finding(wf.name, job_id, 16, "does not create a draft (gh release create --draft) and then "
                                                        "publish it (gh release edit --draft=false)"))
            last = max([i for i, argv in writes if gh_release(argv)[0] in ("create", "upload")], default=-1)
            if public and min(public) <= last:
                out.append(Finding(wf.name, job_id, 16, f"makes the release public in step {min(public) + 1}, not "
                                                        f"after every file is uploaded (step {last + 1})"))
        pushed = [ref for _i, argv in commands for ref in image_refs(argv)]
        if pushed and job_id not in ("publish-image", "image-tags"):
            out.append(Finding(wf.name, job_id, 16, f"pushes {', '.join(pushed)}: only publish-image and image-tags "
                                                    f"push the image"))
        for _i, argv in commands:
            if tool_of(argv) in OTHER_PUSHERS:
                out.append(Finding(wf.name, job_id, 16, f"runs {tool_of(argv)}: release.yml pushes images with docker "
                                                        f"push and docker buildx imagetools create alone, the forms "
                                                        f"rule 16 reads"))
        if job_id in ("publish-image", "image-tags"):
            for ref in pushed:
                name, _, tag = ref.rpartition(":") if ":" in ref.split("/")[-1] else (ref, "", "")
                if not IMAGE_NAME_REF.fullmatch(name):
                    out.append(Finding(wf.name, job_id, 16, f"pushes {ref}: every image release.yml pushes is "
                                                            f"$IMAGE_NAME, the repository package.yml's image job "
                                                            f"tags"))
                elif job_id == "publish-image" and not PUBLISH_TAG.fullmatch(tag):
                    out.append(Finding(wf.name, job_id, 16, f"pushes {ref}: publish-image pushes the version's tags "
                                                            f"alone, exactly :$VERSION or :$VERSION-<arch>; :latest "
                                                            f"and :<major.minor> are image-tags', after the release "
                                                            f"is published"))
        if job_id == "publish-image":
            out += publish_image_version(wf, job)
    return out


def publish_image_version(wf: Workflow, job: dict) -> list[Finding]:
    """publish-image's VERSION is meta's, set once at the job's level: no step and no command sets it (or
    IMAGE_NAME) again, so ``:$VERSION`` is the release's version and nothing else."""
    out = []
    env = job.get("env") if isinstance(job.get("env"), dict) else {}
    if re.sub(r"\s+", "", str(env.get("VERSION", ""))) != META_VERSION:
        out.append(Finding(wf.name, "publish-image", 16, f"its env VERSION is {env.get('VERSION')!r}, not "
                                                         f"${{{{ needs.meta.outputs.version }}}}: the tags it pushes "
                                                         f"are the release's version"))
    if "IMAGE_NAME" in env:
        out.append(Finding(wf.name, "publish-image", 16, "sets IMAGE_NAME again: the workflow's is the one package.yml "
                                                         "tags"))
    held = {"VERSION", "IMAGE_NAME"}
    for i, step in enumerate(wf.steps(job)):
        named = set(step["env"]) & held if isinstance(step.get("env"), dict) else set()
        if named:
            out.append(Finding(wf.name, "publish-image", 16, f"{step_label(i, step)} sets {', '.join(sorted(named))} "
                                                             f"in its env: the job's are meta's version and the "
                                                             f"workflow's image"))
    for i, step in enumerate(wf.steps(job)):
        if not isinstance(step.get("run"), str):
            continue
        for words in shell_commands(step["run"]):
            assigned = {w.split("=", 1)[0] for w in words if re.fullmatch(r"(VERSION|IMAGE_NAME)=.*", w, re.DOTALL)}
            if words[:1] == ["for"]:  # for NAME in ...: the loop's variable
                assigned |= set(words[1:2]) & held
            elif words[:1] == ["read"]:  # read [options] NAME...: each name it sets
                assigned |= {w for w in words[1:] if not w.startswith("-")} & held
            if assigned:
                out.append(Finding(wf.name, "publish-image", 16, f"{step_label(i, step)} assigns "
                                                                 f"{', '.join(sorted(assigned))}: the job's are "
                                                                 f"meta's version and the workflow's image"))
    return out


def release_attestations(wf: Workflow) -> list[Finding]:
    """attest-files attests the folder pkg-release-assets fills, once final has finished it with the sums and the
    signature; attest-image attests publish-image's digest in the registry."""
    out = []
    job = wf.jobs.get("attest-files")
    if isinstance(job, dict):
        steps = wf.steps(job)
        downloads = {str((s.get("with") or {}).get("name")): str((s.get("with") or {}).get("path", ".")).rstrip("/")
                     for s in steps if action_of(s.get("uses")) == "actions/download-artifact"}
        folder = downloads.get("pkg-release-assets")
        attest = [(i, s) for i, s in enumerate(steps) if action_of(s.get("uses")) == "actions/attest-build-provenance"]
        finished = [i for i, argv in run_commands(wf, job)
                    if any(re.split(r"[\\/]", w)[-1] == "assemble_release.py" for w in argv) and "final" in argv
                    and folder is not None and "--release" in argv
                    and argv[argv.index("--release") + 1:argv.index("--release") + 2] == [folder]]
        if folder is None or "rel-release-sums" not in downloads:
            out.append(Finding(wf.name, "attest-files", 16, "does not download pkg-release-assets and rel-release-sums "
                                                            "(the files, and the sums and signature) by name"))
        elif not attest:
            out.append(Finding(wf.name, "attest-files", 16, "has no actions/attest-build-provenance step"))
        else:
            for i, step in attest:
                paths = [p.strip().rstrip("/") for p in str((step.get("with") or {}).get("subject-path", ""))
                         .splitlines() if p.strip()]
                if not any(p in (folder, f"{folder}/*", f"{folder}/**") for p in paths):
                    out.append(Finding(wf.name, "attest-files", 16, f"{step_label(i, step)} attests {paths}, not the "
                                                                    f"folder pkg-release-assets fills ({folder}/*)"))
                if not finished or min(finished) > i:
                    out.append(Finding(wf.name, "attest-files", 16, f"{step_label(i, step)} comes before "
                                                                    f"assemble_release.py final --release {folder} "
                                                                    f"(the sums and the signature joining the files)"))
    job = wf.jobs.get("attest-image")
    if isinstance(job, dict):
        attest = [s for s in wf.steps(job) if action_of(s.get("uses")) == "actions/attest-build-provenance"]
        given = (attest[0].get("with") or {}) if attest else {}
        if (len(attest) != 1 or given.get("push-to-registry") not in (True, "true")
                or "needs.publish-image.outputs.digest" not in re.sub(r"\s+", "", str(given.get("subject-digest")))):
            out.append(Finding(wf.name, "attest-image", 16, "does not attest publish-image's digest "
                                                            "(subject-digest) with push-to-registry: true"))
    return out


def check(root: Path) -> tuple[list[Workflow], list[Finding]]:
    workflows, findings = load(root)
    for wf in workflows:
        for rule in (rule_1, rule_2_3, rule_5, rule_6, rule_7, rule_8, rule_9, rule_10, rule_11, rule_12, rule_14,
                     rule_15, rule_16):
            findings += rule(wf)
    findings += rule_4(workflows)
    findings += rule_13(workflows)
    return workflows, sorted(set(findings), key=lambda f: (f.file, f.job or "", f.rule, f.message))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT, help="the checkout (default: this script's)")
    args = parser.parse_args(argv)
    if yaml is None:
        print("check_workflows: PyYAML is not installed (pip install pyyaml)", file=sys.stderr)
        return 2
    if not (args.root / WORKFLOWS).is_dir():
        print(f"check_workflows: no {WORKFLOWS.as_posix()} under {args.root}", file=sys.stderr)
        return 2
    workflows, findings = check(args.root)
    for finding in findings:
        print(finding)
    jobs = sum(len(wf.jobs) for wf in workflows)
    if findings:
        print(f"check_workflows: {len(findings)} finding(s) in {len(workflows)} workflows ({jobs} jobs)",
              file=sys.stderr)
        return 1
    print(f"check_workflows: {len(workflows)} workflows, {jobs} jobs: rules 1 to 16 hold")
    return 0


if __name__ == "__main__":
    sys.exit(main())
