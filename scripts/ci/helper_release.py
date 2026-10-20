"""The helper's release builds, made ready and proved on the runner that built them (design/phase5-release.md
P5.4.1, package 5-03).

    python scripts/ci/helper_release.py glibc [--max 2.28] [--objdump PATH] BINARY...
    python scripts/ci/helper_release.py macos --min 11.0 [--otool PATH] BINARY...
    python scripts/ci/helper_release.py stage --self FILE --packaged FILE --platform P --version V --out DIR
    python scripts/ci/helper_release.py listing ARCHIVE --platform P --version V
    python scripts/ci/helper_release.py check --stage DIR --platform P --target T --version V --label L --work DIR
                                              [--under DIR] [--keys FILE] [--macos-min 11.0] --out FILE
    python scripts/ci/helper_release.py manifest MANIFEST --assets DIR --version V

Each release target is built twice (package.yml's ``helpers`` matrix): the self-updating helper, which the Servers
page links, ``install.sh --worker`` installs and the manifest lists, and the ``packaged`` one, which Homebrew, Scoop
and winget install and which never updates itself (P4.3.8).

- ``glibc``: the newest ``GLIBC_`` version each binary's dynamic symbols need (``objdump -T``), refused when it is
  above ``--max`` (the game's floor, manylinux_2_28's glibc 2.28: the Linux helpers are built in its container so a
  machine that runs the game runs the helper), or when a binary names no glibc version or a private one.
- ``macos``: the oldest macOS each binary says it runs on (``otool -l``: its ``LC_BUILD_VERSION``'s ``minos``, or an
  older linker's ``LC_VERSION_MIN_MACOSX``), which must be ``--min`` exactly: ``MACOSX_DEPLOYMENT_TARGET``, 11.0 for
  both Macs as for the wheels, so a change that dropped or overrode it (rustc's own default for x86_64 is 10.12) fails.
- ``stage``: the self-updating build under its unversioned name (P4.10: ``citar-helper-<platform>``, ``.exe`` on
  Windows) and the packaged one in its versioned archive, ``citar-helper-<V>-<platform>.tar.gz`` (``.zip`` on
  Windows), holding the binary and the notices that travel with it, each file with its ``.sha256`` (the hex digest
  and a newline) beside it. The archives are reproducible: a fixed order, time and owner. The listing is then checked.
- ``listing``: an archive holds exactly its files, flat, the binary executable.
- ``check``: both builds as ``stage`` left them in ``--stage`` run here, outside any container, on this runner's own
  system as a user's machine would run them, every run through ``gate_run.py helper`` with a home of its own under
  ``--work``: ``--version`` names V, the
  label and the target (one line, exactly); ``--self-test`` passes; ``update check`` says what the tree's
  ``update-keys.pub`` makes it (no key: updates unavailable) and the policy, ``auto`` for the self-updating build,
  ``notify`` for the packaged one, which refuses ``auto`` in its ``helper.json`` (and ``update apply``). Both builds
  report one build id and one jobs build. On a Mac both are held to ``--macos-min`` as ``macos`` holds them (required
  there, read nowhere else). The packaged binary is the one unpacked from the archive. The JSON report
  (``--out``) is what package 5-08's ``assemble`` holds every helper to: one version, label, build and jobs build.
- ``manifest``: ``cargo xtask helper-manifest``'s file over the five, read back: version V, a build and a protocol,
  and each target's release name, its URL in the release's download folder, and the SHA-256 and size of the file in
  ``--assets`` and of its ``.sha256`` (the hash its own runner wrote).

Exit status 0 when every check holds, 1 when one does not (each named), 2 when the command cannot run.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import time
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve()
ROOT = HERE.parents[2]


def _beside(name: str):
    """A script beside this one, as a module (the scripts are not a package)."""
    spec = importlib.util.spec_from_file_location(f"citar_ci_{name}", HERE.parent / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


#: The one way a release helper runs here (gate_run.py helper).
gate_run = sys.modules.get("citar_ci_gate_run") or _beside("gate_run")

#: The platforms and their Rust targets (P4.10's names; scripts/release.py's HELPER_PLATFORMS, xtask's TARGETS).
TARGETS = {"windows-x64": "x86_64-pc-windows-msvc", "macos-arm64": "aarch64-apple-darwin",
           "macos-x64": "x86_64-apple-darwin", "linux-x64": "x86_64-unknown-linux-gnu",
           "linux-arm64": "aarch64-unknown-linux-gnu"}
#: The notices every packaged archive carries beside the binary: each one's name in the archive, and its file in the
#: checkout. THIRD-PARTY-LICENSES.txt is every crate the helper links, with its licence texts (P5.5: cargo xtask
#: licenses writes it, and rust.yml and package.yml's notices job hold it to Cargo.lock).
ARCHIVE_TEXTS = {"LICENSE": "LICENSE", "NOTICE.md": "NOTICE.md",
                 "THIRD-PARTY-LICENSES.txt": "packaging/THIRD-PARTY-LICENSES.txt"}
#: Every archive member's time: 1980-01-01, the earliest a zip can hold.
ARCHIVE_TIME = 315532800
#: The glibc the game's wheels need (manylinux_2_28), and so the helper's floor.
GLIBC_FLOOR = "2.28"
GLIBC = re.compile(r"\bGLIBC_([0-9]+(?:\.[0-9]+)+|PRIVATE)\b")
VERSION_LINE = re.compile(r"^citar-helper (?P<version>\S+) \(build (?P<build>[0-9a-f]{12}), jobs build "
                          r"(?P<jobs>[0-9a-f]{12}), protocol (?P<protocol>\d+), target (?P<target>\S+), "
                          r"(?P<label>[^)]+)\)$")


class Failed(Exception):
    """A check that does not hold: exit 1."""


class Unusable(Exception):
    """The command cannot run: exit 2."""


def windows(platform: str) -> bool:
    return platform.startswith("windows")


def asset_name(platform: str) -> str:
    """The self-updating build's release name (P4.10)."""
    return f"citar-helper-{platform}" + (".exe" if windows(platform) else "")


def archive_name(version: str, platform: str) -> str:
    """The packaged build's archive (P5.4.1; scripts/release.py's ``helper-packaged`` group)."""
    return f"citar-helper-{version}-{platform}" + (".zip" if windows(platform) else ".tar.gz")


def binary_name(platform: str) -> str:
    return "citar-helper" + (".exe" if windows(platform) else "")


def archive_members(platform: str) -> list[str]:
    return [binary_name(platform), *ARCHIVE_TEXTS]


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 16), b""):
            h.update(block)
    return h.hexdigest()


def write_sha256(path: Path) -> Path:
    out = path.with_name(path.name + ".sha256")
    out.write_text(sha256(path) + "\n", encoding="ascii", newline="\n")
    return out


def platform_of(value: str) -> str:
    if value not in TARGETS:
        raise argparse.ArgumentTypeError(f"{value} is not one of {', '.join(TARGETS)}")
    return value


# ------------------------------------------------------------------------------------------------ glibc


def version_key(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in v.split("."))


def glibc_needs(objdump_text: str) -> dict[str, list[str]]:
    """Each glibc version ``objdump -T``'s table names, with the symbols that need it."""
    needs: dict[str, list[str]] = {}
    for line in objdump_text.splitlines():
        m = GLIBC.search(line)
        if not m:
            continue
        symbol = line.split()[-1] if line.split() else "?"
        needs.setdefault(m.group(1), []).append(symbol)
    return needs


def glibc_problem(needs: dict[str, list[str]], floor: str) -> tuple[str | None, str]:
    """The newest version needed, and why that is refused (or None)."""
    if "PRIVATE" in needs:
        symbols = ", ".join(sorted(set(needs["PRIVATE"]))[:5])
        return None, f"it needs GLIBC_PRIVATE ({symbols}), which ties it to one build of glibc"
    versions = sorted(needs, key=version_key)
    if not versions:
        return None, "it names no glibc version: not a binary linked against glibc"
    newest = versions[-1]
    over = [v for v in versions if version_key(v) > version_key(floor)]
    if over:
        symbols = sorted({s for v in over for s in needs[v]})
        return newest, (f"it needs glibc {newest}, above the floor {floor} (GLIBC_{', GLIBC_'.join(over)}: "
                        f"{', '.join(symbols[:8])}{', ...' if len(symbols) > 8 else ''})")
    return newest, ""


def glibc(binaries: list[Path], floor: str, objdump: str) -> list[str]:
    """One line a binary; raises Failed naming each that needs more than ``floor``."""
    lines, problems = [], []
    for binary in binaries:
        try:
            out = subprocess.run([objdump, "-T", str(binary)], capture_output=True, text=True, timeout=120)
        except OSError as e:
            raise Unusable(f"cannot run {objdump}: {e}") from e
        if out.returncode != 0:
            raise Unusable(f"{objdump} -T {binary}: {out.stderr.strip() or out.returncode}")
        newest, why = glibc_problem(glibc_needs(out.stdout), floor)
        if why:
            problems.append(f"{binary}: {why}")
        else:
            lines.append(f"{binary}: needs glibc {newest} at most (the floor is {floor})")
    if problems:
        raise Failed("; ".join(problems))
    return lines


# ------------------------------------------------------------------------------------------------ macOS

#: The macOS both Mac helpers are built for (``MACOSX_DEPLOYMENT_TARGET`` in package.yml and helper.yml).
MACOS_FLOOR = "11.0"


def macos_minimums(otool_text: str) -> list[str]:
    """The oldest macOS each slice of a binary names, from ``otool -l``: ``minos`` in an ``LC_BUILD_VERSION`` load
    command, or ``version`` in an ``LC_VERSION_MIN_MACOSX`` one (what a linker writes for a target before 10.14)."""
    found, command = [], None
    for line in otool_text.splitlines():
        words = line.split()
        if len(words) == 2 and words[0] == "cmd":
            command = words[1]
        elif len(words) == 2 and ((command == "LC_BUILD_VERSION" and words[0] == "minos")
                                  or (command == "LC_VERSION_MIN_MACOSX" and words[0] == "version")):
            found.append(words[1])
            command = None
    return found


def macos(binaries: list[Path], want: str, otool: str | list[str] = "otool") -> list[str]:
    """One line a binary; raises Failed naming each whose oldest macOS is not ``want`` exactly."""
    lines, problems = [], []
    program = [otool] if isinstance(otool, str) else list(otool)
    for binary in binaries:
        try:
            out = subprocess.run([*program, "-l", str(binary)], capture_output=True, text=True, timeout=120)
        except OSError as e:
            raise Unusable(f"cannot run {program[0]}: {e}") from e
        if out.returncode != 0:
            raise Unusable(f"{program[0]} -l {binary}: {out.stderr.strip() or out.returncode}")
        found = macos_minimums(out.stdout)
        if not found:
            problems.append(f"{binary}: names no oldest macOS (no LC_BUILD_VERSION or LC_VERSION_MIN_MACOSX)")
        elif set(found) != {want}:
            problems.append(f"{binary}: runs on macOS {', '.join(sorted(set(found)))} and later, not {want} (is "
                            f"MACOSX_DEPLOYMENT_TARGET {want} where it was built?)")
        else:
            lines.append(f"{binary}: runs on macOS {want} and later")
    if problems:
        raise Failed("; ".join(problems))
    return lines


# ------------------------------------------------------------------------------------------------ the archives


def pack(binary: Path, platform: str, version: str, out: Path, root: Path = ROOT) -> Path:
    """The packaged build's archive in ``out``: the binary (executable) and the notices, flat, reproducible."""
    members = [(binary_name(platform), binary, 0o755)]
    members += [(name, root / source, 0o644) for name, source in ARCHIVE_TEXTS.items()]
    for _name, source, _mode in members:
        if not source.is_file():
            raise Unusable(f"{source} is missing")
    path = out / archive_name(version, platform)
    if windows(platform):
        with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as z:
            for name, source, mode in members:
                info = zipfile.ZipInfo(name, date_time=time.gmtime(ARCHIVE_TIME)[:6])
                info.create_system = 3  # Unix, so the mode below is read
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                z.writestr(info, source.read_bytes())
    else:
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as t:
            for name, source, mode in members:
                data = source.read_bytes()
                info = tarfile.TarInfo(name)
                info.size, info.mode, info.mtime = len(data), mode, ARCHIVE_TIME
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                t.addfile(info, io.BytesIO(data))
        with path.open("wb") as f, gzip.GzipFile(filename="", mode="wb", fileobj=f, mtime=0) as g:
            g.write(raw.getvalue())
    return path


def listing(archive: Path, platform: str, version: str) -> list[str]:
    """The archive's members, checked: exactly its files, flat, regular, the binary executable."""
    want = archive_members(platform)
    if archive.name != archive_name(version, platform):
        raise Failed(f"{archive.name} is not {archive_name(version, platform)}")
    entries: list[tuple[str, int, bool]] = []
    try:
        if windows(platform):
            with zipfile.ZipFile(archive) as z:
                for info in z.infolist():
                    entries.append((info.filename, (info.external_attr >> 16) & 0o777, not info.is_dir()))
        else:
            with tarfile.open(archive, "r:gz") as t:
                for info in t.getmembers():
                    entries.append((info.name, info.mode & 0o777, info.isreg()))
    except (OSError, zipfile.BadZipFile, tarfile.TarError) as e:
        raise Failed(f"{archive} does not read: {e}") from e
    names = [name for name, _mode, _regular in entries]
    if names != want:
        raise Failed(f"{archive.name} holds {names}, not exactly {want}")
    for name, mode, regular in entries:
        if not regular:
            raise Failed(f"{archive.name}: {name} is not a regular file")
        if name == binary_name(platform) and not mode & 0o100:
            raise Failed(f"{archive.name}: {name} is not executable (mode {mode:o})")
    return [f"{name} {mode:o}" for name, mode, _regular in entries]


def unpack_binary(archive: Path, platform: str, into: Path) -> Path:
    """The binary from the archive, as a user's unpacking gives it."""
    into.mkdir(parents=True, exist_ok=True)
    out = into / binary_name(platform)
    if windows(platform):
        with zipfile.ZipFile(archive) as z:
            out.write_bytes(z.read(binary_name(platform)))
    else:
        with tarfile.open(archive, "r:gz") as t:
            member = t.getmember(binary_name(platform))
            source = t.extractfile(member)
            if source is None:
                raise Failed(f"{archive.name}: {member.name} has no data")
            out.write_bytes(source.read())
            out.chmod(member.mode & 0o777)
    return out


def stage(self_build: Path, packaged: Path, platform: str, version: str, out: Path) -> list[str]:
    out.mkdir(parents=True, exist_ok=True)
    asset = out / asset_name(platform)
    shutil.copyfile(self_build, asset)
    asset.chmod(0o755)
    archive = pack(packaged, platform, version, out)
    lines = []
    for path in (asset, archive):
        write_sha256(path)
        lines.append(f"{path.name} {path.stat().st_size} bytes, sha256 {sha256(path)}")
    lines += [f"  {line}" for line in listing(archive, platform, version)]
    return lines


# ------------------------------------------------------------------------------------------------ the check


def key_count(path: Path) -> int:
    """How many public keys a key file names (``update-keys.pub``: one a line, blank and ``#`` lines read past)."""
    text = path.read_text(encoding="utf-8") if path.is_file() else ""
    # distinct, as citar-jobs reads the file: a key given twice is trusted once
    return len({line.strip() for line in text.splitlines() if line.strip() and not line.strip().startswith("#")})


class Runner:
    """Runs one binary through gate_run.py helper, each run's output kept for the report."""

    def __init__(self, binary: Path, under: Path, timeout: float):
        self.binary, self.under, self.timeout = binary, under, timeout
        self.runs: list[dict] = []

    def __call__(self, *args: str, expect: int | None = 0) -> str:
        done = gate_run.run_helper(self.binary, list(args), self.under, timeout=self.timeout)
        out = (done.stdout or "") + (done.stderr or "")
        self.runs.append({"args": list(args), "exit": done.returncode})
        if expect is not None and done.returncode != expect:
            raise Failed(f"{self.binary.name} {' '.join(args)} exited {done.returncode}, not {expect}:\n{out.strip()}")
        print(f"helper_release: {self.binary.name} {' '.join(args)}: exit {done.returncode}")
        for line in out.strip().splitlines():
            print(f"  {line}")
        return out


def read_version(out: str, what: str, version: str, target: str, label: str) -> dict:
    lines = [line for line in out.splitlines() if line.startswith("citar-helper ")]
    if len(lines) != 1:
        raise Failed(f"{what} --version printed {len(lines)} version lines: {out.strip()}")
    m = VERSION_LINE.match(lines[0].strip())
    if not m:
        raise Failed(f"{what} --version does not read: {lines[0]}")
    got = {"version": m["version"], "target": m["target"], "label": m["label"]}
    want = {"version": version, "target": target, "label": label}
    if got != want:
        raise Failed(f"{what} says {got}, not {want}: {lines[0]}")
    return {"line": lines[0].strip(), "build": m["build"], "jobs_build": m["jobs"], "protocol": int(m["protocol"])}


def updates_line(out: str, what: str) -> str:
    lines = [line for line in out.splitlines() if line.startswith("updates: ")]
    if len(lines) != 1:
        raise Failed(f"{what} update check printed no single updates line: {out.strip()}")
    return lines[0].strip()


def check(staged: Path, platform: str, target: str, version: str, label: str, work: Path, under: Path, keys: Path,
          timeout: float, macos_min: str | None = None, otool: str | list[str] = "otool") -> dict:
    began = time.perf_counter()
    if TARGETS[platform] != target:
        raise Failed(f"{platform}'s target is {TARGETS[platform]}, not {target}")
    mac = platform.startswith("macos")
    if mac and not macos_min:
        raise Unusable(f"{platform}: give --macos-min, the MACOSX_DEPLOYMENT_TARGET both builds must name")
    under = gate_run.check_under(under)
    work = gate_run.inside(work, under, "--work")
    self_build, archive = staged / asset_name(platform), staged / archive_name(version, platform)
    for path in (self_build, archive):
        if not path.is_file():
            raise Failed(f"{path} is missing: stage puts both builds there")
        side = path.with_name(path.name + ".sha256")
        if not side.is_file() or side.read_text(encoding="ascii").strip() != sha256(path):
            raise Failed(f"{side.name} is missing or is not {path.name}'s")
    listing(archive, platform, version)
    packaged = unpack_binary(archive, platform, work / "packaged")
    trusted = key_count(keys)
    homes = {name: work / f"home-{name}" for name in ("self", "packaged", "packaged-auto")}
    for home in homes.values():
        if home.exists() and any(home.iterdir()):
            raise Unusable(f"{home} is not empty: give a fresh --work")
        home.mkdir(parents=True, exist_ok=True)

    report: dict = {"platform": platform, "target": target, "version": version, "label": label,
                    "keys_trusted": trusted}
    if mac:
        for line in macos([self_build, packaged], macos_min, otool):
            print(f"helper_release: {line}")
        report["macos_min"] = macos_min
    for what, binary in (("self", self_build), ("packaged", packaged)):
        run = Runner(binary, under, timeout)
        home = str(homes[what])
        facts = read_version(run("--version", "--home", home), what, version, target, label)
        run("--self-test", "--home", home)
        out = run("update", "check", "--home", home)
        if not re.search(rf"^update keys trusted: {trusted}\b", out, re.MULTILINE):
            raise Failed(f"{what} update check does not trust the tree's {trusted} key(s): {out.strip()}")
        line = updates_line(out, what)
        policy = "auto" if what == "self" else "notify (a packaged build: its package manager updates it)"
        want = (f"updates: unavailable, this build trusts no update key; policy: {policy}" if trusted == 0
                else f"updates: {policy}")
        if line != want:
            raise Failed(f"{what} update check says {line!r}, not {want!r}")
        if what == "packaged":
            auto = homes["packaged-auto"]
            (auto / "helper.json").write_text('{"updates": "auto"}\n', encoding="utf-8")
            line = updates_line(run("update", "check", "--home", str(auto)), "packaged with updates auto")
            if "auto, which a packaged build refuses: update it through your package manager" not in line:
                raise Failed(f"the packaged build with updates auto in its helper.json says {line!r}")
            out = run("update", "apply", "--home", str(auto), expect=2)
            if "package manager" not in out:
                raise Failed(f"the packaged build's update apply was not refused for its package manager: {out}")
        source = self_build if what == "self" else archive
        report[what] = {**facts, "file": source.name, "sha256": sha256(source), "size": source.stat().st_size,
                        "runs": run.runs}
    if (report["self"]["build"], report["self"]["jobs_build"]) != (report["packaged"]["build"],
                                                                    report["packaged"]["jobs_build"]):
        raise Failed(f"the two builds disagree: {report['self']['line']} / {report['packaged']['line']}")
    report.update(build=report["self"]["build"], jobs_build=report["self"]["jobs_build"],
                  protocol=report["self"]["protocol"], seconds=round(time.perf_counter() - began, 1))
    return report


# ------------------------------------------------------------------------------------------------ the manifest

#: Where a release's assets are downloaded from (xtask's DEFAULT_BASE_URL).
RELEASE_URL = "https://github.com/jprodgers/CITAR/releases/download/v{version}/"


def check_manifest(manifest: Path, assets: Path, version: str) -> list[str]:
    """The manifest read back against the five files it lists and their ``.sha256``."""
    try:
        doc = json.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise Failed(f"{manifest} does not read: {e}") from e
    if not isinstance(doc, dict) or doc.get("v") != 1 or doc.get("version") != version:
        raise Failed(f"{manifest} is not version 1 of a manifest for {version}: v {doc.get('v')!r}, version "
                     f"{doc.get('version')!r}")
    if not re.fullmatch(r"[0-9a-f]{12}", str(doc.get("build"))) or type(doc.get("protocol")) is not int:
        raise Failed(f"{manifest} has no build or protocol: {doc.get('build')!r}, {doc.get('protocol')!r}")
    listed = doc.get("assets")
    want = {target: asset_name(platform) for platform, target in TARGETS.items()}
    if not isinstance(listed, dict) or set(listed) != set(want):
        raise Failed(f"{manifest} lists {sorted(listed or {})}, not the five targets {sorted(want)}")
    lines = [f"{manifest.name}: {version}, build {doc['build']}, protocol {doc['protocol']}"]
    for target, name in sorted(want.items()):
        entry, path = listed[target], assets / name
        if not path.is_file():
            raise Failed(f"{path} is missing")
        side = path.with_name(name + ".sha256")
        recorded = side.read_text(encoding="ascii").strip() if side.is_file() else None
        got = {"name": entry.get("name"), "url": entry.get("url"), "sha256": entry.get("sha256"),
               "size": entry.get("size")}
        expected = {"name": name, "url": RELEASE_URL.format(version=version) + name, "sha256": sha256(path),
                    "size": path.stat().st_size}
        if got != expected:
            raise Failed(f"{manifest.name}'s {target}: {got}, not {expected}")
        if recorded != expected["sha256"]:
            raise Failed(f"{side.name} says {recorded}, not {name}'s {expected['sha256']}")
        lines.append(f"  {target}: {name}, {expected['size']} bytes, sha256 {expected['sha256'][:16]}..., as its "
                     f".sha256 says")
    return lines


# ------------------------------------------------------------------------------------------------ command line


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="helper_release.py", description=__doc__.split("\n\n", 1)[0])
    sub = ap.add_subparsers(dest="command", required=True)
    g = sub.add_parser("glibc")
    g.add_argument("--max", default=GLIBC_FLOOR)
    g.add_argument("--objdump", default="objdump")
    g.add_argument("binaries", nargs="+", type=Path)
    mo = sub.add_parser("macos")
    mo.add_argument("--min", dest="minimum", required=True)
    mo.add_argument("--otool", default="otool")
    mo.add_argument("binaries", nargs="+", type=Path)
    s = sub.add_parser("stage")
    s.add_argument("--self", dest="self_build", type=Path, required=True)
    s.add_argument("--packaged", type=Path, required=True)
    s.add_argument("--platform", type=platform_of, required=True)
    s.add_argument("--version", required=True)
    s.add_argument("--out", type=Path, required=True)
    li = sub.add_parser("listing")
    li.add_argument("archive", type=Path)
    li.add_argument("--platform", type=platform_of, required=True)
    li.add_argument("--version", required=True)
    c = sub.add_parser("check")
    c.add_argument("--stage", type=Path, required=True)
    c.add_argument("--platform", type=platform_of, required=True)
    c.add_argument("--target", required=True)
    c.add_argument("--version", required=True)
    c.add_argument("--label", required=True)
    c.add_argument("--work", type=Path, required=True)
    c.add_argument("--under", type=Path, default=None)
    c.add_argument("--keys", type=Path, default=ROOT / "crates" / "citar-jobs" / "update-keys.pub")
    c.add_argument("--timeout", type=float, default=300.0)
    c.add_argument("--macos-min", default=None, help="the oldest macOS the Mac builds must name (macOS only)")
    c.add_argument("--out", type=Path, required=True)
    m = sub.add_parser("manifest")
    m.add_argument("manifest", type=Path)
    m.add_argument("--assets", type=Path, required=True)
    m.add_argument("--version", required=True)
    args = ap.parse_args(argv)
    try:
        if args.command == "glibc":
            for line in glibc(args.binaries, args.max, args.objdump):
                print(f"helper_release: {line}")
        elif args.command == "macos":
            for line in macos(args.binaries, args.minimum, args.otool):
                print(f"helper_release: {line}")
        elif args.command == "stage":
            for line in stage(args.self_build, args.packaged, args.platform, args.version, args.out):
                print(f"helper_release: {line}")
        elif args.command == "listing":
            for line in listing(args.archive, args.platform, args.version):
                print(line)
        elif args.command == "manifest":
            for line in check_manifest(args.manifest, args.assets, args.version):
                print(f"helper_release: {line}")
        else:
            under = args.under or Path(os.environ.get("CARGO_TARGET_DIR") or os.environ.get("RUNNER_TEMP") or ".")
            report = check(args.stage, args.platform, args.target, args.version, args.label, args.work, under,
                           args.keys, args.timeout, args.macos_min)
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.write_text(json.dumps(report, sort_keys=True) + "\n", encoding="utf-8", newline="\n")
            print(f"helper_release: both builds of {args.platform} hold: build {report['build']}, jobs build "
                  f"{report['jobs_build']}, label {args.label} ({report['seconds']} s); {args.out}")
    except gate_run.Refused as e:
        print(f"helper_release: refused: {e}", file=sys.stderr)
        return 2
    except Unusable as e:
        print(f"helper_release: {e}", file=sys.stderr)
        return 2
    except Failed as e:
        print(f"helper_release: FAILED: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
