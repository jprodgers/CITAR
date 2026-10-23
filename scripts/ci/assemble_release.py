"""A release assembled from package.yml's artifacts, and every build in it held to one (design/phase5-release.md
P5.10, package 5-08).

    python scripts/ci/assemble_release.py assemble --version V --label L --release DIR --reports DIR --preview DIR
    python scripts/ci/assemble_release.py sums --version V --release DIR [--signed]

package.yml's ``assemble`` job downloads the release's files by their artifacts' exact names into one folder
(``--release``) and the builds' reports into another (``--reports``), then runs ``assemble``:

1. **The files.** ``--release`` must hold exactly the release's assets (``scripts/release.py assets V``, P5.10's
   names), each with its ``.sha256`` beside it where its build wrote one: a missing asset, an extra file (or folder)
   and a ``.sha256`` that is not its file's are each named. The ``.sha256`` files the builds do not write (the
   wheels', the sdist's, the constraints', the manifest's and the notices') are written here.
2. **One build.** The reports must be exactly these, each named when missing or unexpected: the five wheels' smoke
   lines on Python 3.11 and 3.14 (``smoke-<platform>-<py>.json``, ``scripts/ci/smoke_wheel.py``), the sdist build's
   (``smoke-sdist.json``), the helpers' (``helper-<platform>.json``, both builds of each, ``helper_release.py
   check``), the bundle's (``windows-bundle.json``, with ``windows-installed.json`` and ``windows-uninstalled.json``
   beside it), both images' (``image-<arch>.json``, ``check_image.py``) and the install scripts'
   (``install-<platform>.json``, ``install_check.py``). Every one must report the version V and the label L, except
   the sdist build's, which must report the label ``unknown`` (a user's build carries none) and is not compared with
   L; all of them one build id and one jobs build (so the server's job host and every released helper can share CPU
   work, P4.4.5), the helpers one protocol; the ten wheel lines, the sdist build's and the images' smoke one result
   hash and one spec hash for the seeded lab job. Where values differ, each report that differs from the most common
   value is named. Each report must also be of what this folder holds: a wheel line names its platform's wheel, a
   helper report its two files with their SHA-256 (so the files released are the ones tested), every install check
   passed.
3. **SHA256SUMS** over every asset, as ``sha256sum`` writes it (``sha256sum -c SHA256SUMS`` accepts it), then the
   folder must be exactly ``release.py assets V``'s files.
4. **The preview** (``--preview``): ``notes.md`` (V's CHANGELOG section, ``release.py notes``, then the checksums, how
   to check a file's provenance with ``gh attestation verify``, and the ways to install), ``SHA256SUMS``,
   ``subjects.txt`` (every file of the release, the attestation's subjects, one a line), ``assets.json`` (each asset
   with its group, size and SHA-256), ``build.json`` (the one build: V, L, the build id, jobs build, protocol, result
   and spec hashes, and each report's values), and ``managers/`` (``scripts/release_checksums.py``'s manifests from
   these files, with the release's real URLs).

``sums`` (re)writes ``SHA256SUMS`` over a release folder that holds exactly ``release.py assets V [--signed]``'s
assets, writing any ``.sha256`` missing beside one and refusing one that differs: release.yml's own step once the
manifest's signature (``--signed``) joins the files.

Standard library only. Exit status 0 when everything holds; 1 when something does not (each named, all of them); 2 for
a usage error.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve()
ROOT = HERE.parents[2]


def _load(name: str, path: Path):
    """A script as a module (the scripts are not a package)."""
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module      # dataclasses look their module up while a class is made
    spec.loader.exec_module(module)
    return module


release = _load("citar_release_for_checksums", ROOT / "scripts" / "release.py")
checksums = _load("citar_release_checksums", ROOT / "scripts" / "release_checksums.py")
helper_release = _load("citar_ci_helper_release", HERE.parent / "helper_release.py")
smoke_wheel = _load("citar_ci_smoke_wheel", HERE.parent / "smoke_wheel.py")

SUMS = "SHA256SUMS"
#: The interpreters every wheel is smoke-tested on (P5.3.4: the floor and the newest).
SMOKE_PYTHONS = ("3.11", "3.14")
#: The image's architectures and the wheel platform each is built from (P5.7).
IMAGES = {"amd64": "linux-x64", "arm64": "linux-arm64"}
#: The bundle's report, and the two that come beside it in pkg-windows-report (read, never compared).
BUNDLE = "windows-bundle.json"
BUNDLE_BESIDE = ("windows-installed.json", "windows-uninstalled.json")
#: The label a build from the sdist carries: a user's build sets no CITAR_BUILD_ID (P5.3.3).
UNKNOWN = "unknown"
HEX64 = re.compile(r"[0-9a-f]{64}")


class Problems(Exception):
    """Things that do not hold, each naming its file."""

    def __init__(self, problems: list[str]):
        super().__init__(f"{len(problems)} problem(s)")
        self.problems = problems


class Usage(Exception):
    """The command cannot run as given: exit 2."""


def say(text: str) -> None:
    print(f"assemble_release: {text}", flush=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def side_hash(path: Path) -> str | None:
    """The hash a ``.sha256`` file states (its first word: the builds write the hex digest and a newline)."""
    words = path.read_text(encoding="ascii", errors="replace").split()
    return words[0].lower() if words else None


def wheel_names(version: str) -> dict[str, str]:
    """Each platform's wheel (P5.3.2: release.py's WHEEL_TAGS in HELPER_PLATFORMS' order)."""
    pep440 = release.Version.parse(version).pep440
    return {platform: f"citar-{pep440}-cp311-abi3-{tag}.whl"
            for platform, tag in zip(release.HELPER_PLATFORMS, release.WHEEL_TAGS)}


# ------------------------------------------------------------------------------------------------ 1. the files


def check_files(folder: Path, version: str, signed: bool = False) -> list[str]:
    """Problems with the folder's set: missing assets, extra files, a ``.sha256`` that is not its file's."""
    if not folder.is_dir():
        return [f"{folder}: no such folder"]
    names = [a["name"] for a in release.assets(version, signed)]
    allowed = set(names) | {f"{n}.sha256" for n in names} | {SUMS}
    present = {p.name: p for p in folder.iterdir()}
    problems = [f"{n}: missing (an asset of {version})" for n in names if n not in present]
    for name in sorted(present):
        if name not in allowed:
            problems.append(f"{name}: extra (no asset of {version}, nor its .sha256)")
        elif not present[name].is_file():
            problems.append(f"{name}: not a file")
    for name in names:
        side = present.get(f"{name}.sha256")
        if side is not None and name in present and present[name].is_file() and side.is_file():
            stated = side_hash(side)
            if stated != sha256(present[name]):
                problems.append(f"{name}.sha256: says {stated}, and {name}'s SHA-256 is {sha256(present[name])}")
    return problems


def write_sides(folder: Path, version: str, signed: bool = False) -> list[str]:
    """The ``.sha256`` beside every asset that has none (the hex digest and a newline, as the builds write it)."""
    written = []
    for asset in release.assets(version, signed):
        side = folder / asset["sha256"]
        if not side.exists():
            side.write_text(sha256(folder / asset["name"]) + "\n", encoding="ascii", newline="\n")
            written.append(side.name)
    return written


def write_sums(folder: Path, version: str, signed: bool = False) -> str:
    """SHA256SUMS over every asset, in P5.10's order, as ``sha256sum`` writes it."""
    lines = "".join(f"{sha256(folder / a['name'])}  {a['name']}\n" for a in release.assets(version, signed))
    (folder / SUMS).write_text(lines, encoding="ascii", newline="\n")
    return lines


def check_final(folder: Path, version: str, signed: bool = False) -> list[str]:
    want = release.asset_files(version, signed)
    have = sorted(p.name for p in folder.iterdir())
    problems = [f"{n}: missing after assembly" for n in want if n not in have]
    problems += [f"{n}: extra after assembly" for n in have if n not in want]
    return problems


# ------------------------------------------------------------------------------------------------ 2. one build


def expected_reports() -> dict[str, tuple]:
    """Every report ``assemble`` reads, file name -> what it is."""
    out: dict[str, tuple] = {}
    for platform in release.HELPER_PLATFORMS:
        for py in SMOKE_PYTHONS:
            out[f"smoke-{platform}-{py}.json"] = ("wheel", platform, py)
    out["smoke-sdist.json"] = ("sdist",)
    for platform in release.HELPER_PLATFORMS:
        out[f"helper-{platform}.json"] = ("helper", platform)
    out[BUNDLE] = ("bundle",)
    for arch in IMAGES:
        out[f"image-{arch}.json"] = ("image", arch)
    for platform in release.HELPER_PLATFORMS:
        out[f"install-{platform}.json"] = ("install", platform)
    return out


def read_report(path: Path) -> dict:
    """A report's JSON object: a smoke line is one line, the others one document."""
    text = path.read_text(encoding="utf-8")
    data = json.loads(text)
    if not isinstance(data, dict):
        raise ValueError("not a JSON object")
    return data


class Facts:
    """What every report says, to be held to one value each."""

    def __init__(self):
        self.problems: list[str] = []
        self.values: dict[str, list[tuple[str, object]]] = collections.defaultdict(list)
        self.sources: list[dict] = []

    def need(self, ok: bool, problem: str) -> bool:
        if not ok:
            self.problems.append(problem)
        return ok

    def one(self, key: str, file: str, value) -> None:
        self.values[key].append((file, value))

    def agree(self, key: str, what: str) -> object:
        """The most common value of ``key``; each file that says another is a problem."""
        seen = self.values.get(key, [])
        if not seen:
            return None
        counts = collections.Counter(str(v) for _, v in seen)
        best = counts.most_common(1)[0][0]
        value = next(v for _, v in seen if str(v) == best)
        for file, v in seen:
            if str(v) != best:
                self.problems.append(f"{file}: {what} {v}, and the release's other builds say {best}")
        return value


def smoke_facts(facts: Facts, file: str, data: dict, kind: tuple, version: str, label: str, wheels: dict) -> None:
    missing = [k for k in smoke_wheel.RECORD_KEYS if k not in data]
    if not facts.need(not missing, f"{file}: the smoke line lacks {', '.join(missing)}"):
        return
    facts.need(data["version"] == version, f"{file}: version {data['version']}, not {version}")
    facts.need(data["verifiable"] is True, f"{file}: its job host was not verifiable")
    if kind[0] == "wheel":
        platform, py = kind[1], kind[2]
        facts.need(data["label"] == label, f"{file}: label {data['label']}, not {label}")
        facts.need(data["platform"] == platform, f"{file}: platform {data['platform']}, not {platform}")
        facts.need(str(data["python"]).startswith(py + "."), f"{file}: Python {data['python']}, not {py}")
        facts.need(data["wheel"] == wheels[platform], f"{file}: the wheel {data['wheel']}, not the release's "
                                                      f"{wheels[platform]}")
    else:
        # A user's build carries no label: the sdist's must say unknown, and is never compared with the release's.
        facts.need(data["label"] == UNKNOWN, f"{file}: label {data['label']}, not {UNKNOWN} (a build from the sdist "
                                             f"carries no label)")
    facts.one("build", file, data["build_id"])
    facts.one("jobs", file, data["jobs_build"])
    facts.one("result", file, data["result_hash"])
    facts.one("spec", file, data["spec_hash"])


def helper_facts(facts: Facts, file: str, data: dict, platform: str, version: str, label: str,
                 folder: Path) -> None:
    for key in ("platform", "target", "version", "label", "build", "jobs_build", "protocol", "self", "packaged"):
        if not facts.need(key in data, f"{file}: no {key}"):
            return
    facts.need(data["platform"] == platform, f"{file}: platform {data['platform']}, not {platform}")
    target = helper_release.TARGETS[platform]
    facts.need(data["target"] == target, f"{file}: target {data['target']}, not {target}")
    facts.need(data["version"] == version, f"{file}: version {data['version']}, not {version}")
    facts.need(data["label"] == label, f"{file}: label {data['label']}, not {label}")
    files = {"self": helper_release.asset_name(platform),
             "packaged": helper_release.archive_name(version, platform)}
    for build, name in files.items():
        part = data[build]
        if not facts.need(isinstance(part, dict) and "line" in part, f"{file}: no {build} build's version line"):
            continue
        m = helper_release.VERSION_LINE.match(str(part["line"]))
        if not facts.need(m is not None, f"{file}: the {build} build's version line does not read: {part['line']}"):
            continue
        facts.need(m["version"] == version and m["label"] == label and m["target"] == target,
                   f"{file}: the {build} build says {m['version']}, {m['label']}, {m['target']}")
        facts.one("build", f"{file} ({build})", m["build"])
        facts.one("jobs", f"{file} ({build})", m["jobs"])
        facts.one("protocol", f"{file} ({build})", int(m["protocol"]))
        facts.need(part.get("file") == name, f"{file}: the {build} build is {part.get('file')}, not {name}")
        if (folder / name).is_file():
            facts.need(part.get("sha256") == sha256(folder / name),
                       f"{file}: the {build} build tested ({part.get('sha256')}) is not the release's {name}")


def install_facts(facts: Facts, file: str, data: dict, platform: str, version: str, label: str) -> None:
    facts.need(data.get("platform") == platform, f"{file}: platform {data.get('platform')}, not {platform}")
    facts.need(data.get("version") == version, f"{file}: version {data.get('version')}, not {version}")
    facts.need(data.get("label") == label, f"{file}: label {data.get('label')}, not {label}")
    checks = data.get("checks")
    if facts.need(isinstance(checks, list) and checks, f"{file}: no checks"):
        failed = [str(c.get("check", "?")) if isinstance(c, dict) else repr(c) for c in checks
                  if not (isinstance(c, dict) and c.get("ok") is True)]
        facts.need(not failed, f"{file}: checks that did not pass: {'; '.join(failed)}")
    doctor = (data.get("game") or {}).get("doctor") or {}
    if facts.need(bool(doctor), f"{file}: no citar doctor engine line"):
        facts.need(doctor.get("version") == version and doctor.get("label") == label,
                   f"{file}: citar doctor names {doctor.get('version')}, {doctor.get('label')}")
        facts.one("build", f"{file} (citar doctor)", doctor.get("build"))
    line = (data.get("helper") or {}).get("line")
    m = helper_release.VERSION_LINE.match(str(line or ""))
    if facts.need(m is not None, f"{file}: no installed helper's version line"):
        facts.need(m["version"] == version and m["label"] == label,
                   f"{file}: the installed helper says {m['version']}, {m['label']}")
        facts.one("build", f"{file} (helper)", m["build"])
        facts.one("jobs", f"{file} (helper)", m["jobs"])


def check_reports(reports: Path, version: str, label: str, folder: Path) -> tuple[list[str], dict]:
    """Every report there and none other, each of V and L (the sdist build's of unknown), all of one build."""
    facts = Facts()
    if not reports.is_dir():
        return [f"{reports}: no such folder"], {}
    expected = expected_reports()
    present = {p.name for p in reports.iterdir()}
    for name in expected:
        facts.need(name in present, f"{name}: missing (a report assemble reads)")
    for name in sorted(present - set(expected) - set(BUNDLE_BESIDE)):
        facts.problems.append(f"{name}: extra (no report assemble reads)")
    wheels = wheel_names(version)
    for name, kind in expected.items():
        if name not in present:
            continue
        try:
            data = read_report(reports / name)
        except (OSError, ValueError) as e:
            facts.problems.append(f"{name}: does not read: {e}")
            continue
        facts.sources.append({"file": name, "kind": kind[0], **{k: data.get(k) for k in (
            "platform", "version", "label", "build_id", "build", "jobs_build", "result_hash")}})
        if kind[0] in ("wheel", "sdist"):
            smoke_facts(facts, name, data, kind, version, label, wheels)
        elif kind[0] == "helper":
            helper_facts(facts, name, data, kind[1], version, label, folder)
        elif kind[0] == "install":
            install_facts(facts, name, data, kind[1], version, label)
        else:      # the bundle, an image
            for key in ("version", "label", "build_id", "jobs_build"):
                facts.need(key in data, f"{name}: no {key}")
            facts.need(data.get("version") == version, f"{name}: version {data.get('version')}, not {version}")
            facts.need(data.get("label") == label, f"{name}: label {data.get('label')}, not {label}")
            facts.one("build", name, data.get("build_id"))
            facts.one("jobs", name, data.get("jobs_build"))
            if kind[0] == "image":
                smoke = data.get("smoke")
                if facts.need(isinstance(smoke, dict) and "result_hash" in smoke, f"{name}: no smoke line"):
                    facts.one("result", f"{name} (smoke)", smoke["result_hash"])
                    facts.one("spec", f"{name} (smoke)", smoke.get("spec_hash"))
    one = {"version": version, "label": label,
           "build_id": facts.agree("build", "build id"),
           "jobs_build": facts.agree("jobs", "jobs build"),
           "protocol": facts.agree("protocol", "helper protocol"),
           "result_hash": facts.agree("result", "the seeded lab job's result hash"),
           "spec_hash": facts.agree("spec", "the seeded lab job's spec hash"),
           "sources": facts.sources}
    return facts.problems, one


# ------------------------------------------------------------------------------------------------ 4. the preview


def install_routes(version: str) -> str:
    v = release.Version.parse(version)
    raw = "https://raw.githubusercontent.com/jprodgers/CITAR/main"
    return "\n".join([
        "## Installing",
        "",
        f"- **pip or pipx** (wheels for Windows x64, macOS 11 or newer on Apple silicon and Intel, Linux x64 and "
        f"arm64 with glibc 2.28 or newer; Python 3.11 or newer): `pipx install \"citar[all]=={v.pep440}\"`.",
        f"- **macOS and Linux:** `curl -fsSL {raw}/install.sh | bash`.",
        f"- **Windows:** `CITAR-{version}-setup.exe` (per user, no administrator rights needed), the portable "
        f"`CITAR-{version}-windows-x64.zip`, or in PowerShell "
        f"`& ([scriptblock]::Create((iwr -useb {raw}/install.ps1)))`.",
        "- **Scoop:** `scoop bucket add citar https://github.com/jprodgers/scoop-citar`, then `scoop install citar` "
        "(and `scoop install citar-helper`).",
        "- **winget:** `winget install JimmieRodgers.CITAR` and `winget install JimmieRodgers.CITAR.Helper`, once "
        "winget's repository has this version.",
        f"- **The server image:** `docker pull ghcr.io/jprodgers/citar:{version}` (linux/amd64 and linux/arm64).",
        "- **The CITAR helper:** the `citar-helper-<platform>` file for your computer, which updates itself; "
        f"`curl -fsSL {raw}/install.sh | bash -s -- --worker`, or `-Worker` after install.ps1's one-liner; or the "
        "packaged build through Homebrew (`brew install jprodgers/citar/citar-helper`), Scoop or winget, which "
        "update it instead.",
        "",
    ])


def notes(root: Path, version: str, sums: str) -> str:
    body = release.notes(root, version).rstrip("\n")
    return "\n".join([
        body,
        "",
        "## Checksums",
        "",
        "Each file's SHA-256, also in `SHA256SUMS` and beside each file as `<file>.sha256`. Check a download with "
        "`sha256sum -c SHA256SUMS --ignore-missing` (Linux), `shasum -a 256 -c SHA256SUMS --ignore-missing` "
        "(macOS) or `Get-FileHash <file>` (PowerShell).",
        "",
        "```",
        sums.rstrip("\n"),
        "```",
        "",
        "## Where each file was built",
        "",
        "Every file of this release was built and attested by this repository's release workflow. Check one with "
        "the GitHub CLI:",
        "",
        "```",
        "gh attestation verify <file> --repo jprodgers/CITAR",
        "```",
        "",
        install_routes(version),
    ])


def write_preview(root: Path, folder: Path, preview: Path, version: str, label: str, one: dict,
                  sums: str) -> list[str]:
    preview.mkdir(parents=True, exist_ok=True)
    (preview / "notes.md").write_text(notes(root, version, sums), encoding="utf-8", newline="\n")
    (preview / SUMS).write_text(sums, encoding="ascii", newline="\n")
    files = release.asset_files(version)
    (preview / "subjects.txt").write_text("".join(f"{n}\n" for n in files), encoding="utf-8", newline="\n")
    v = release.Version.parse(version)
    assets = [{**a, "size": (folder / a["name"]).stat().st_size, "sha256_hex": sha256(folder / a["name"])}
              for a in release.assets(version)]
    (preview / "assets.json").write_text(json.dumps({
        "version": version, "pep440": v.pep440, "label": label, "assets": assets, "files": files,
        "bytes": sum((folder / n).stat().st_size for n in files)}, indent=2) + "\n", encoding="utf-8", newline="\n")
    (preview / "build.json").write_text(json.dumps(one, indent=2) + "\n", encoding="utf-8", newline="\n")
    names = checksums.needed(root, version)
    hashes = checksums.hashes_from_dir(folder, names)
    date = checksums.release_date(root, version, None)
    written = checksums.write_manifests(root, version, date, hashes, checksums.base_url(None, version),
                                        preview / "managers")
    return ["notes.md", SUMS, "subjects.txt", "assets.json", "build.json",
            *(str(p.relative_to(preview)).replace("\\", "/") for p in written)]


# ------------------------------------------------------------------------------------------------ the commands


def assemble(root: Path, folder: Path, reports: Path, preview: Path, version: str, label: str) -> dict:
    problems = check_files(folder, version)
    report_problems, one = check_reports(reports, version, label, folder)
    problems += report_problems
    if problems:
        raise Problems(problems)
    for name in write_sides(folder, version):
        say(f"wrote {name}")
    sums = write_sums(folder, version)
    problems = check_final(folder, version)
    if problems:
        raise Problems(problems)
    say(f"one build: {version}, {label}, build {one['build_id']}, jobs build {one['jobs_build']}, protocol "
        f"{one['protocol']}, result hash {one['result_hash']} ({len(one['sources'])} reports)")
    say(f"{SUMS}: {len(sums.splitlines())} assets; the folder is release.py assets {version}'s "
        f"{len(release.asset_files(version))} files")
    for name in write_preview(root, folder, preview, version, label, one, sums):
        say(f"preview: {name}")
    return one


def sums_command(folder: Path, version: str, signed: bool) -> str:
    problems = check_files(folder, version, signed)
    if problems:
        raise Problems(problems)
    for name in write_sides(folder, version, signed):
        say(f"wrote {name}")
    sums = write_sums(folder, version, signed)
    problems = check_final(folder, version, signed)
    if problems:
        raise Problems(problems)
    return sums


def main(argv: list[str] | None = None) -> int:
    top = argparse.ArgumentParser(prog="assemble_release.py", description=__doc__.split("\n\n")[0],
                                  allow_abbrev=False)
    sub = top.add_subparsers(dest="command", required=True)
    p = sub.add_parser("assemble", allow_abbrev=False, help="check, sum and preview a release folder")
    p.add_argument("--version", required=True)
    p.add_argument("--label", required=True, help="the release builds' label (release.py meta)")
    p.add_argument("--release", type=Path, required=True, help="the release's files")
    p.add_argument("--reports", type=Path, required=True, help="the builds' reports")
    p.add_argument("--preview", type=Path, required=True, help="where the notes, sums and manifests go")
    p.add_argument("--root", type=Path, default=ROOT, help="the checkout (default: this script's)")
    p = sub.add_parser("sums", allow_abbrev=False, help="(re)write SHA256SUMS over a release folder")
    p.add_argument("--version", required=True)
    p.add_argument("--release", type=Path, required=True)
    p.add_argument("--signed", action="store_true", help="the folder holds the manifest's signature too")
    args = top.parse_args(argv)
    try:
        version = release.Version.parse(args.version).text
        if args.command == "assemble":
            if not args.label or args.label != args.label.strip():
                raise Usage(f"--label {args.label!r}: the label release.py meta gave")
            preview = args.preview.resolve()
            folder = args.release.resolve()
            if preview == folder or folder in preview.parents:
                raise Usage("--preview must be outside --release: the release holds its assets and nothing else")
            assemble(args.root.resolve(), folder, args.reports.resolve(), preview, version, args.label)
            say(f"{version} assembled in {folder}")
        else:
            sums = sums_command(args.release.resolve(), version, args.signed)
            say(f"{SUMS}: {len(sums.splitlines())} assets")
    except (release.Refused, Usage) as e:
        print(f"assemble_release: {e}", file=sys.stderr)
        return 2
    except Problems as e:
        for problem in e.problems:
            print(f"assemble_release: FAILED: {problem}", file=sys.stderr)
        print(f"assemble_release: {len(e.problems)} problem(s)", file=sys.stderr)
        return 1
    except checksums.Problem as e:
        print(f"assemble_release: FAILED: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
