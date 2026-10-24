"""The package managers' manifests checked, and installed from a loopback copy of the release on CI's runners
(design/phase5-release.md P5.9, package 5-08).

    python scripts/ci/check_managers.py structure --dir DIR --version V [--assets DIR] [--base-url URL]
    python scripts/ci/check_managers.py winget-schemas --dir DIR [--schemas DIR]
    python scripts/ci/check_managers.py scoop --release DIR --version V --work DIR [--under DIR] --out FILE
    python scripts/ci/check_managers.py winget --release DIR --manifests DIR --version V --work DIR [--under DIR]
                                              --out FILE
    python scripts/ci/check_managers.py brew --release DIR --version V --work DIR [--under DIR] --out FILE

``DIR`` of ``structure`` and ``winget-schemas`` holds the manifests ``scripts/release_checksums.py --out`` wrote, in
``packaging/``'s layout (MANIFESTS below).

- ``structure``: every manifest there and none missing. Scoop's JSON parses and has its keys, its URL and hash the
  asset's; each winget file *begins* with its schema header (``# yaml-language-server:
  $schema=https://aka.ms/winget-manifest.<type>.<version>.schema.json``, without which winget refuses the whole
  set), its ``ManifestType`` and ``ManifestVersion`` the header's, the fields its type requires, one identifier and
  version across a package's three files, ``Documentations`` spelt so (the published schemas allow unknown fields,
  so ``Documentation`` would validate and be dropped), the installer's URL and SHA-256 the asset's, and no ``Scope``
  on the portable helper (winget supports none there); the formula states no ``version`` (Homebrew reads it from the
  release's URLs, ``.../releases/download/vV/``), has a URL and SHA-256 under each of its four platform blocks
  (``on_macos``/``on_linux`` x ``on_arm``/``on_intel``), each its platform's packaged archive, and installs the binary
  and the notices, with a service and a test. With ``--assets`` every hash is held to the file's.
- ``winget-schemas``: each winget file validated against the JSON schema its header names (``jsonschema``), fetched
  from that URL, or read from ``--schemas DIR`` by the URL's file name; YAML read as winget reads it (a date stays a
  string). PyYAML and jsonschema are this command's only packages.
- ``scoop``, ``winget``, ``brew``: the managers themselves, on CI's runners only (``GITHUB_ACTIONS=true``; nothing
  is installed on the laptop, P5.18.1 rule 1). The release folder is served from a loopback server, the manifests
  are written again pointing there (``release_checksums.py --base-url``), and each package is installed, run
  (``citar --version``; ``citar-helper --version`` through ``gate_run.py helper`` with a home under ``--under``) and
  uninstalled. ``scoop`` installs both of Scoop's packages. ``winget`` runs ``winget validate`` on the real-URL
  manifests (``--manifests``) and ``winget install --manifest`` on the loopback ones, where the runner has winget;
  where it has none it says so as a workflow notice and passes (the schemas are the gate). A winget older than the
  files' ``ManifestVersion`` warns on every schema header (it holds them to its own newest schema); that warning
  alone passes with a notice saying so, and any other warning or error fails. ``brew`` makes a tap of
  its own (``brew tap-new --no-git``), runs ``brew audit --strict`` and ``brew style`` on the formula with the real
  release URLs (offline: nothing is published yet) and reads the version Homebrew finds there (``brew info``), then
  ``brew install`` and ``brew test`` on the loopback copy, which states that version (a loopback URL has no
  ``/releases/download/vV/`` to read it from), checks the notices in its ``doc``, and uninstalls it and removes the
  tap. Each writes a JSON report.

Exit status 0 when every check holds; 1 when one does not (each named); 2 for a usage error.
"""
from __future__ import annotations

import argparse
import hashlib
import http.server
import importlib.util
import json
import os
import platform as host
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve()
ROOT = HERE.parents[2]


def _load(name: str, path: Path):
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


release = _load("citar_release_for_checksums", ROOT / "scripts" / "release.py")
checksums = _load("citar_release_checksums", ROOT / "scripts" / "release_checksums.py")
helper_release = _load("citar_ci_helper_release", HERE.parent / "helper_release.py")

#: Every manifest, by its path under the manifests' folder (packaging/'s layout): its kind, and for a winget file its
#: schema type, its package and the asset it installs (``{v}`` the version).
MANIFESTS = {
    "homebrew/citar-helper.rb": ("formula", None, None, None),
    "scoop/citar.json": ("scoop", None, None, "CITAR-{v}-windows-x64.zip"),
    "scoop/citar-helper.json": ("scoop", None, None, "citar-helper-{v}-windows-x64.zip"),
    "winget/game/JimmieRodgers.CITAR.yaml": ("winget", "version", "JimmieRodgers.CITAR", None),
    "winget/game/JimmieRodgers.CITAR.installer.yaml": ("winget", "installer", "JimmieRodgers.CITAR",
                                                       "CITAR-{v}-setup.exe"),
    "winget/game/JimmieRodgers.CITAR.locale.en-US.yaml": ("winget", "defaultLocale", "JimmieRodgers.CITAR", None),
    "winget/helper/JimmieRodgers.CITAR.Helper.yaml": ("winget", "version", "JimmieRodgers.CITAR.Helper", None),
    "winget/helper/JimmieRodgers.CITAR.Helper.installer.yaml": ("winget", "installer", "JimmieRodgers.CITAR.Helper",
                                                                "citar-helper-{v}-windows-x64.zip"),
    "winget/helper/JimmieRodgers.CITAR.Helper.locale.en-US.yaml": ("winget", "defaultLocale",
                                                                   "JimmieRodgers.CITAR.Helper", None),
}
#: winget's schema header, the first line of every file.
WINGET_HEADER = re.compile(r"# yaml-language-server: \$schema=(https://aka\.ms/winget-manifest\.(installer|"
                           r"defaultLocale|locale|version|singleton)\.(\d+\.\d+\.\d+)\.schema\.json)")
#: The fields each winget type requires (the 1.12.0 schemas' ``required``; the installer's type may be the root's).
WINGET_REQUIRED = {
    "installer": ("PackageIdentifier", "PackageVersion", "Installers", "ManifestType", "ManifestVersion"),
    "defaultLocale": ("PackageIdentifier", "PackageVersion", "PackageLocale", "Publisher", "PackageName", "License",
                      "ShortDescription", "ManifestType", "ManifestVersion"),
    "version": ("PackageIdentifier", "PackageVersion", "DefaultLocale", "ManifestType", "ManifestVersion"),
}
INSTALLER_REQUIRED = ("Architecture", "InstallerUrl", "InstallerSha256")
#: The formula's four platform blocks and the packaged archive each installs.
FORMULA_BLOCKS = {("macos", "arm"): "macos-arm64", ("macos", "intel"): "macos-x64",
                  ("linux", "arm"): "linux-arm64", ("linux", "intel"): "linux-x64"}
OUTER_BLOCK = re.compile(r"^  on_(macos|linux) do\n(.*?)^  end\n", re.M | re.S)
INNER_BLOCK = re.compile(r"^    on_(arm|intel) do\n((?:^      .*\n)*?)^    end\n", re.M)
FORMULA_URL = re.compile(r'^      url "([^"]+)"$', re.M)
FORMULA_SHA = re.compile(r'^      sha256 "([0-9a-f]{64})"$', re.M)
FORMULA_VERSION = re.compile(r'^\s+version\s+"', re.M)
FORMULA_HOMEPAGE = re.compile(r'^  homepage "[^"]*"\n', re.M)
HEX64 = re.compile(r"[0-9a-fA-F]{64}")
TIMEOUT = 600.0


class Failed(Exception):
    """A check that did not hold."""


class Usage(Exception):
    """The command cannot run as given: exit 2."""


def say(text: str) -> None:
    print(f"check_managers: {text}", flush=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def yaml_load(text: str):
    """YAML as winget reads it against its schema: a date such as ``ReleaseDate: 2026-11-01`` stays a string."""
    import yaml

    class Loader(yaml.SafeLoader):
        pass

    Loader.yaml_implicit_resolvers = {
        first: [(tag, regexp) for tag, regexp in resolvers if tag != "tag:yaml.org,2002:timestamp"]
        for first, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()}
    return yaml.load(text, Loader=Loader)


# ------------------------------------------------------------------------------------------------ structure


class Expect:
    """What the manifests must point at: the base URL and, with ``--assets``, each asset's hash."""

    def __init__(self, version: str, base: str, assets: Path | None):
        self.version, self.base, self.assets = version, base, assets
        self._hashes: dict[str, str] = {}

    def name(self, template: str) -> str:
        return template.format(v=self.version)

    def url(self, name: str) -> str:
        return self.base + name

    def sha(self, name: str) -> str | None:
        if self.assets is None:
            return None
        if name not in self._hashes:
            path = self.assets / name
            if not path.is_file():
                raise Failed(f"--assets {self.assets} lacks {name}")
            self._hashes[name] = sha256(path)
        return self._hashes[name]


def check_scoop(rel: str, text: str, asset: str, expect: Expect) -> list[str]:
    problems = []
    try:
        data = json.loads(text)
    except ValueError as e:
        return [f"{rel}: does not parse as JSON: {e}"]
    for key in ("version", "description", "homepage", "license", "architecture", "bin", "checkver", "autoupdate"):
        if key not in data:
            problems.append(f"{rel}: no {key!r}")
    if data.get("version") != expect.version:
        problems.append(f"{rel}: version {data.get('version')!r}, not {expect.version}")
    arch = ((data.get("architecture") or {}).get("64bit")) or {}
    name = expect.name(asset)
    if arch.get("url") != expect.url(name):
        problems.append(f"{rel}: url {arch.get('url')!r}, not {expect.url(name)}")
    want = expect.sha(name)
    if not HEX64.fullmatch(str(arch.get("hash", ""))) or arch.get("hash") != arch.get("hash", "").lower():
        problems.append(f"{rel}: hash {arch.get('hash')!r} is no lower-case SHA-256")
    elif want is not None and arch.get("hash") != want:
        problems.append(f"{rel}: hash {arch.get('hash')}, and {name}'s SHA-256 is {want}")
    if rel.endswith("/citar.json") and data.get("extract_dir") != "CITAR":
        problems.append(f"{rel}: extract_dir {data.get('extract_dir')!r}, not 'CITAR' (the zip's folder)")
    if rel.endswith("/citar-helper.json") and data.get("bin") != "citar-helper.exe":
        problems.append(f"{rel}: bin {data.get('bin')!r}, not 'citar-helper.exe'")
    return problems


def check_winget(rel: str, text: str, kind: str, package: str, asset: str | None, expect: Expect) -> list[str]:
    problems = []
    first = text.split("\n", 1)[0].rstrip("\r")
    m = WINGET_HEADER.fullmatch(first)
    if not m:
        return [f"{rel}: does not begin with its schema header (# yaml-language-server: $schema=https://aka.ms/"
                f"winget-manifest.<type>.<version>.schema.json): {first!r}"]
    if m[2] != kind:
        problems.append(f"{rel}: its header names the {m[2]} schema, not {kind}")
    try:
        data = yaml_load(text)
    except Exception as e:      # PyYAML's errors share no base class worth naming here
        return problems + [f"{rel}: does not parse as YAML: {e}"]
    if not isinstance(data, dict):
        return problems + [f"{rel}: not a mapping"]
    for key in WINGET_REQUIRED[kind]:
        if key not in data:
            problems.append(f"{rel}: no {key} (the {kind} schema requires it)")
    if data.get("ManifestType") != kind:
        problems.append(f"{rel}: ManifestType {data.get('ManifestType')!r}, not {kind}")
    if str(data.get("ManifestVersion")) != m[3]:
        problems.append(f"{rel}: ManifestVersion {data.get('ManifestVersion')!r}, and its header's {m[3]}")
    if data.get("PackageIdentifier") != package:
        problems.append(f"{rel}: PackageIdentifier {data.get('PackageIdentifier')!r}, not {package}")
    if str(data.get("PackageVersion")) != expect.version:
        problems.append(f"{rel}: PackageVersion {data.get('PackageVersion')!r}, not {expect.version}")
    if kind == "defaultLocale":
        if "Documentation" in data:
            problems.append(f"{rel}: 'Documentation' is no winget field: it is spelt 'Documentations'")
        docs = data.get("Documentations")
        if not (isinstance(docs, list) and docs and all(isinstance(d, dict) and d.get("DocumentUrl") for d in docs)):
            problems.append(f"{rel}: no Documentations list with a DocumentUrl in each")
    if kind == "installer":
        installers = data.get("Installers") or []
        if not data.get("InstallerType") and not all(i.get("InstallerType") for i in installers):
            problems.append(f"{rel}: no InstallerType")
        if len(installers) != 1:
            problems.append(f"{rel}: {len(installers)} installers, not one (x64)")
        name = expect.name(asset)
        for i in installers:
            for key in INSTALLER_REQUIRED:
                if key not in i:
                    problems.append(f"{rel}: an installer without {key}")
            if i.get("InstallerUrl") != expect.url(name):
                problems.append(f"{rel}: InstallerUrl {i.get('InstallerUrl')!r}, not {expect.url(name)}")
            got = str(i.get("InstallerSha256", ""))
            want = expect.sha(name)
            if not HEX64.fullmatch(got):
                problems.append(f"{rel}: InstallerSha256 {got!r} is no SHA-256")
            elif want is not None and got.lower() != want:
                problems.append(f"{rel}: InstallerSha256 {got}, and {name}'s SHA-256 is {want.upper()}")
        if data.get("InstallerType") == "zip":
            nested = data.get("NestedInstallerFiles") or []
            if data.get("NestedInstallerType") != "portable" or not nested or \
                    nested[0].get("RelativeFilePath") != "citar-helper.exe" or \
                    nested[0].get("PortableCommandAlias") != "citar-helper":
                problems.append(f"{rel}: not the portable citar-helper.exe with the command alias citar-helper")
            if "Scope" in data or any("Scope" in i for i in installers):
                problems.append(f"{rel}: a Scope, which winget does not support for a portable installer (winget "
                                f"validate warns)")
    return problems


def check_formula(rel: str, text: str, expect: Expect) -> list[str]:
    problems = []
    for needle in ('class CitarHelper < Formula', 'bin.install "citar-helper"',
                   'doc.install "LICENSE", "NOTICE.md", "THIRD-PARTY-LICENSES.txt"', "  service do\n",
                   '    run [opt_bin/"citar-helper", "run", "--service"]', "  test do\n", "--self-test", "--version"):
        if needle not in text:
            problems.append(f"{rel}: lacks {needle.strip()!r}")
    if FORMULA_VERSION.search(text):
        problems.append(f"{rel}: states a version, which Homebrew reads from the release's URLs "
                        f"(.../releases/download/v{expect.version}/); brew audit --strict refuses it as redundant")
    found: dict[tuple[str, str], tuple[list[str], list[str]]] = {}
    for outer in OUTER_BLOCK.finditer(text):
        body = outer[2] if outer[2].endswith("\n") else outer[2] + "\n"
        for inner in INNER_BLOCK.finditer(body):
            key = (outer[1], inner[1])
            if key in found:
                problems.append(f"{rel}: on_{key[0]} has two on_{key[1]} blocks")
            found[key] = (FORMULA_URL.findall(inner[2]), FORMULA_SHA.findall(inner[2]))
    for key, platform in FORMULA_BLOCKS.items():
        where = f"on_{key[0]} / on_{key[1]}"
        if key not in found:
            problems.append(f"{rel}: no {where} block")
            continue
        urls, shas = found[key]
        name = helper_release.archive_name(expect.version, platform)
        if urls != [expect.url(name)]:
            problems.append(f"{rel}: {where}'s url is {urls}, not [{expect.url(name)}]")
        want = expect.sha(name)
        if len(shas) != 1:
            problems.append(f"{rel}: {where} has {len(shas)} sha256 lines, not one")
        elif want is not None and shas[0] != want:
            problems.append(f"{rel}: {where}'s sha256 is {shas[0]}, and {name}'s is {want}")
    for key in found:
        if key not in FORMULA_BLOCKS:
            problems.append(f"{rel}: an unexpected block on_{key[0]} / on_{key[1]}")
    if "files.pythonhosted.org" in text or "pypi.org" in text:
        problems.append(f"{rel}: names PyPI (the helper is a release file)")
    return problems


def structure(folder: Path, version: str, base: str, assets: Path | None) -> list[str]:
    expect = Expect(version, base, assets)
    problems = []
    present = sorted(str(p.relative_to(folder)).replace("\\", "/") for p in folder.rglob("*") if p.is_file())
    for rel in present:
        if rel not in MANIFESTS:
            problems.append(f"{rel}: no manifest this check knows")
    for rel, (kind, schema, package, asset) in MANIFESTS.items():
        path = folder / rel
        if not path.is_file():
            problems.append(f"{rel}: missing")
            continue
        text = path.read_text(encoding="utf-8")
        if "{{" in text or "}}" in text:
            problems.append(f"{rel}: a placeholder was left unfilled")
        if re.search(r"files\.pythonhosted\.org|/packages/source/", text):
            problems.append(f"{rel}: names PyPI's sdist URL, which 404s until PyPI has caught up")
        try:
            if kind == "scoop":
                problems += check_scoop(rel, text, asset, expect)
            elif kind == "winget":
                problems += check_winget(rel, text, schema, package, asset, expect)
            else:
                problems += check_formula(rel, text, expect)
        except Failed as e:
            problems.append(f"{rel}: {e}")
    return problems


# ------------------------------------------------------------------------------------------------ winget's schemas


def schema_for(url: str, schemas: Path | None, cache: dict) -> dict:
    if url in cache:
        return cache[url]
    name = url.rsplit("/", 1)[-1]
    if schemas is not None:
        path = schemas / name
        if not path.is_file():
            raise Usage(f"--schemas {schemas} lacks {name} (the schema {url})")
        data = json.loads(path.read_text(encoding="utf-8"))
    else:
        with urllib.request.urlopen(url, timeout=120) as response:
            data = json.loads(response.read().decode("utf-8"))
    cache[url] = data
    return data


def winget_schemas(folder: Path, schemas: Path | None) -> tuple[list[str], list[str]]:
    """Each winget file against the schema its header names: (problems, what was checked)."""
    import jsonschema

    problems, checked, cache = [], [], {}
    for rel, (kind, *_rest) in MANIFESTS.items():
        if kind != "winget":
            continue
        path = folder / rel
        if not path.is_file():
            problems.append(f"{rel}: missing")
            continue
        text = path.read_text(encoding="utf-8")
        m = WINGET_HEADER.fullmatch(text.split("\n", 1)[0].rstrip("\r"))
        if not m:
            problems.append(f"{rel}: no schema header on its first line")
            continue
        schema = schema_for(m[1], schemas, cache)
        validator = jsonschema.validators.validator_for(schema)(schema)
        errors = sorted(validator.iter_errors(yaml_load(text)), key=lambda e: list(e.absolute_path))
        for error in errors:
            where = "/".join(str(p) for p in error.absolute_path) or "(the document)"
            problems.append(f"{rel}: {where}: {error.message}")
        checked.append(f"{rel}: {m[1]} ({len(errors)} error(s))")
    return problems, checked


# ------------------------------------------------------------------------------------------------ the runners


class Served:
    """A folder on a loopback HTTP server, every path asked for kept."""

    def __init__(self, root: Path):
        self.requests: list[str] = []
        requests = self.requests

        class Handler(http.server.SimpleHTTPRequestHandler):
            def __init__(self, *args, **kwargs):
                super().__init__(*args, directory=str(root), **kwargs)

            def log_message(self, format, *args):
                requests.append(self.path)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


class Report:
    def __init__(self, manager: str, version: str):
        self.data = {"manager": manager, "version": version, "platform": f"{host.system()}-{host.machine()}",
                     "checks": []}
        self.began = time.perf_counter()

    def record(self, name: str, ok: bool, detail: str = "") -> bool:
        self.data["checks"].append({"check": name, "ok": ok, "detail": detail})
        print(f"check_managers: {'ok' if ok else 'FAILED'}: {name}{': ' + detail if detail else ''}", flush=True)
        return ok

    def write(self, out: Path) -> int:
        self.data["seconds"] = round(time.perf_counter() - self.began, 1)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(self.data, indent=1) + "\n", encoding="utf-8")
        failed = [c for c in self.data["checks"] if not c["ok"]]
        if failed:
            print(f"check_managers: FAILED: {len(failed)} of {len(self.data['checks'])} checks", file=sys.stderr)
            return 1
        say(f"passed: {len(self.data['checks'])} checks in {self.data['seconds']} s; wrote {out}")
        return 0


def on_a_runner(manager: str) -> None:
    """Scoop, winget and Homebrew install packages: on CI's runners only, never on the laptop (P5.18.1 rule 1)."""
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise Usage(f"{manager} installs packages: it runs on CI's runners only (GITHUB_ACTIONS=true), never on this "
                    f"machine (design/phase5-release.md P5.18.1 rule 1)")


def run(argv: list, what: str, timeout: float = TIMEOUT, env: dict | None = None, cwd: Path | None = None,
        echo: bool = True) -> tuple[int, str]:
    if echo:
        say(f"$ {' '.join(str(a) for a in argv)}")
    try:
        done = subprocess.run([str(a) for a in argv], capture_output=True, text=True, errors="replace",
                              timeout=timeout, env=env, cwd=cwd, stdin=subprocess.DEVNULL)
    except FileNotFoundError as e:
        return 127, f"{what}: {e}"
    except subprocess.TimeoutExpired:
        return 124, f"{what}: no end within {timeout:.0f} s"
    out = (done.stdout or "") + (done.stderr or "")
    # All of a failure's output: its cause can sit in stdout, ahead of a long stderr (brew audit's problems).
    lines = out.strip().splitlines()
    for line in lines if done.returncode != 0 else lines[-60:]:
        print(f"    {line}", flush=True)
    return done.returncode, out


def manifests_for(release_dir: Path, version: str, base: str, out: Path) -> Path:
    """The manifests again, pointing at ``base`` (the loopback copy), hashed from the release's files."""
    names = checksums.needed(ROOT, version)
    hashes = checksums.hashes_from_dir(release_dir, names)
    date = checksums.release_date(ROOT, version, None)
    if out.exists():
        shutil.rmtree(out)
    checksums.write_manifests(ROOT, version, date, hashes, checksums.base_url(base, version), out)
    return out


def helper_runs(report: Report, binary: str | Path, version: str, label: str | None, under: Path, home: Path,
                what: str) -> None:
    """``citar-helper --version`` through gate_run.py helper, a home of its own under ``under``."""
    code, out = run([sys.executable, HERE.parent / "gate_run.py", "helper", "--under", under, "--", binary,
                     "--version", "--home", home], f"{what} --version", timeout=120)
    m = helper_release.VERSION_LINE.match(next((line.strip() for line in out.splitlines()
                                                 if line.startswith("citar-helper ")), ""))
    ok = code == 0 and m is not None and m["version"] == version and (label is None or m["label"] == label)
    report.record(f"{what} runs: citar-helper --version (gate_run.py helper)", ok, m.group(0) if m else out[-400:])


def cmd_scoop(args) -> int:
    on_a_runner("scoop")
    report = Report("scoop", args.version)
    scoop = shutil.which("scoop")
    if not report.record("scoop is on PATH", scoop is not None, str(scoop)):
        return report.write(args.out)
    code, out = run([scoop, "--version"], "scoop --version")
    report.record("scoop --version", code == 0, out.strip().splitlines()[-1] if out.strip() else "")
    served = Served(args.release)
    try:
        manifests = manifests_for(args.release, args.version, served.url, args.work / "loopback")
        for package, asset in (("citar", f"CITAR-{args.version}-windows-x64.zip"),
                               ("citar-helper", f"citar-helper-{args.version}-windows-x64.zip")):
            before = len(served.requests)
            code, out = run([scoop, "install", manifests / "scoop" / f"{package}.json"], f"scoop install {package}")
            fetched = [r for r in served.requests[before:] if r.endswith(asset)]
            report.record(f"scoop install {package} (from the loopback release)", code == 0 and bool(fetched),
                          f"exit {code}, fetched {fetched}")
        code, out = run([scoop, "list"], "scoop list")
        report.record("scoop list names both, at the release's version",
                      code == 0 and all(re.search(rf"^\s*{re.escape(p)}\s+{re.escape(args.version)}\b", out, re.M)
                                        for p in ("citar", "citar-helper")), "")
        citar = shutil.which("citar")
        code, out = run([citar or "citar", "--version"], "citar --version", timeout=300)
        report.record("citar --version (Scoop's shim)", code == 0 and out.strip().endswith(f"CITAR {args.version}"),
                      out.strip()[-200:])
        helper = shutil.which("citar-helper")
        helper_runs(report, helper or "citar-helper", args.version, args.label, args.under,
                    args.work / "helper-home", "Scoop's citar-helper")
    finally:
        served.close()
    for package in ("citar-helper", "citar"):
        code, out = run([scoop, "uninstall", package], f"scoop uninstall {package}")
        report.record(f"scoop uninstall {package}", code == 0, "")
    code, out = run([scoop, "list"], "scoop list")
    report.record("nothing of CITAR is left in scoop list", not re.search(r"^\s*citar(-helper)?\s", out, re.M), "")
    return report.write(args.out)


#: winget validate's one warning that says nothing of the files when the client is older than their ManifestVersion:
#: it holds the header to its own newest schema's ``$id``, so a 1.11 client warns on every 1.12.0 header.
HEADER_WARNING = "The schema header URL does not match the expected pattern"


def version_tuple(text: str) -> tuple[int, int] | None:
    m = re.search(r"(\d+)\.(\d+)", text)
    return (int(m[1]), int(m[2])) if m else None


def manifest_version(folder: Path) -> tuple[int, int] | None:
    """The ManifestVersion of a package's files (their headers'), or None when they disagree or have none."""
    found = set()
    for path in sorted(folder.glob("*.yaml")):
        m = WINGET_HEADER.fullmatch(path.read_text(encoding="utf-8").split("\n", 1)[0].rstrip("\r"))
        found.add(version_tuple(m[3]) if m else None)
    return found.pop() if len(found) == 1 else None


def validate_outcome(code: int, out: str, client: tuple[int, int] | None,
                     manifests: tuple[int, int] | None) -> tuple[bool, str]:
    """Whether ``winget validate`` passed: exit 0; or, on a client older than the files' ManifestVersion, warnings
    that are all the schema header's (which such a client cannot match; the JSON schemas, checked before, are the
    files' real schema check). Any other warning, or any error, fails: (passed, why it passed on warnings)."""
    if code == 0:
        return True, ""
    lines = [line.strip() for line in out.splitlines()]
    warnings = [line for line in lines if line.startswith("Manifest Warning:")]
    if ("Manifest validation succeeded with warnings." in lines and warnings
            and not any(line.startswith("Manifest Error:") for line in lines)
            and all(HEADER_WARNING in line for line in warnings)
            and client is not None and manifests is not None and client < manifests):
        return True, (f"exit {code}: only the schema header warning ({len(warnings)} files), from a winget "
                      f"{client[0]}.{client[1]} older than ManifestVersion {manifests[0]}.{manifests[1]}")
    return False, ""


def cmd_winget(args) -> int:
    on_a_runner("winget")
    report = Report("winget", args.version)
    winget = shutil.which("winget")
    if winget is None:
        # The schemas (winget-schemas) are the gate; winget's own validator and an install run where it exists.
        print("::notice title=winget::winget is not on this runner: winget validate and winget install --manifest "
              "were not run (the JSON schemas, checked above, are the gate)", flush=True)
        report.data["winget"] = None
        return report.write(args.out)
    code, out = run([winget, "--version"], "winget --version")
    report.record("winget --version", code == 0, out.strip()[-80:])
    client = version_tuple(out) if code == 0 else None
    for package in ("game", "helper"):
        folder = args.manifests / "winget" / package
        code, out = run([winget, "validate", "--manifest", folder], f"winget validate {package}")
        passed, why = validate_outcome(code, out, client, manifest_version(folder))
        if why:
            print(f"::notice title=winget validate ({package})::{why}", flush=True)
        report.record(f"winget validate --manifest ({package}, the real URLs)", passed, why or out.strip()[-300:])
    code, out = run([winget, "settings", "--enable", "LocalManifestFiles"], "winget settings")
    report.record("winget settings --enable LocalManifestFiles", code == 0, out.strip()[-200:])
    served = Served(args.release)
    try:
        manifests = manifests_for(args.release, args.version, served.url, args.work / "loopback")
        agree = ["--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"]
        code, out = run([winget, "install", "--manifest", manifests / "winget" / "helper", *agree],
                        "winget install the helper")
        report.record("winget install --manifest (the helper, from the loopback release)", code == 0,
                      out.strip()[-300:])
        links = Path(os.environ.get("LOCALAPPDATA", "")) / "Microsoft" / "WinGet" / "Links" / "citar-helper.exe"
        helper_runs(report, shutil.which("citar-helper") or links, args.version, args.label, args.under,
                    args.work / "helper-home", "winget's citar-helper")
        # By the manifest it was installed from: `--id` finds no installed package for a local manifest's install
        # (the merge's CI). The source agreements again, since uninstall looks in the sources too, msstore's included.
        quiet = ["--accept-source-agreements", "--disable-interactivity"]
        code, out = run([winget, "uninstall", "--manifest", manifests / "winget" / "helper", *quiet],
                        "winget uninstall the helper")
        how = "--manifest"
        if code != 0:
            run([winget, "list", "--name", "CITAR", *quiet], "winget list (what winget sees installed)")
            code, out = run([winget, "uninstall", "--name", "CITAR helper", "--exact", *quiet],
                            "winget uninstall the helper by name")
            how = "--name 'CITAR helper'"
        report.record("winget uninstall JimmieRodgers.CITAR.Helper", code == 0, f"{how}: {out.strip()[-200:]}")
        report.record("winget's citar-helper command is gone", not links.exists(), str(links))
    finally:
        served.close()
    return report.write(args.out)


def find_brew() -> str | None:
    for candidate in (shutil.which("brew"), "/opt/homebrew/bin/brew", "/usr/local/bin/brew",
                      "/home/linuxbrew/.linuxbrew/bin/brew"):
        if candidate and Path(candidate).is_file():
            return candidate
    return None


def brew_problems(out: str) -> str:
    """brew audit's problem lines (``  * ...``), else its output's tail."""
    problems = [line.strip() for line in out.splitlines() if line.lstrip().startswith("* ")]
    return "; ".join(problems) if problems else out.strip()[-600:]


def formula_version(info: str) -> str | None:
    """The stable version in ``brew info --json=v2``'s output (its stdout's JSON, whatever stderr adds), or None."""
    try:
        return json.JSONDecoder().raw_decode(info, info.index("{"))[0]["formulae"][0]["versions"]["stable"]
    except (ValueError, LookupError, TypeError):
        return None


def loopback_formula(text: str, version: str) -> str:
    """The loopback copy with the version stated. The real formula states none: Homebrew reads it from the release's
    URLs (``.../releases/download/vV/``), and ``brew audit --strict`` refuses a stated one as redundant. A loopback URL
    has no such path, and from ``citar-helper-V-macos-arm64.tar.gz`` alone Homebrew reads ``64``."""
    if FORMULA_VERSION.search(text):
        raise Failed("the formula states a version already")
    stated, n = FORMULA_HOMEPAGE.subn(lambda m: f'{m[0]}  version "{version}"\n', text, count=1)
    if n != 1:
        raise Failed("the formula has no homepage line to put the version after")
    return stated


def cmd_brew(args) -> int:
    on_a_runner("brew")
    report = Report("brew", args.version)
    brew = find_brew()
    if not report.record("Homebrew is on this runner", brew is not None, str(brew)):
        return report.write(args.out)
    env = {**os.environ, "HOMEBREW_NO_AUTO_UPDATE": "1", "HOMEBREW_NO_INSTALL_CLEANUP": "1",
           "HOMEBREW_NO_ANALYTICS": "1", "HOMEBREW_NO_ENV_HINTS": "1", "HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK": "1"}
    tap, formula = "citar/dryrun", "citar/dryrun/citar-helper"
    code, out = run([brew, "--version"], "brew --version", env=env)
    report.record("brew --version", code == 0, out.strip().splitlines()[0] if out.strip() else "")
    run([brew, "untap", "--force", tap], "brew untap (a tap left by an earlier run)", env=env)
    code, out = run([brew, "tap-new", "--no-git", tap], "brew tap-new", env=env)
    if not report.record(f"brew tap-new --no-git {tap}", code == 0, out.strip()[-200:]):
        return report.write(args.out)
    code, out = run([brew, "--repository", tap], "brew --repository", env=env)
    tap_dir = Path(out.strip().splitlines()[-1]) if code == 0 and out.strip() else None
    if not report.record("the tap's folder", tap_dir is not None and tap_dir.is_dir(), str(tap_dir)):
        return report.write(args.out)
    (tap_dir / "Formula").mkdir(exist_ok=True)
    target = tap_dir / "Formula" / "citar-helper.rb"
    real = manifests_for(args.release, args.version, checksums.base_url(None, args.version), args.work / "real")
    shutil.copyfile(real / "homebrew" / "citar-helper.rb", target)
    code, out = run([brew, "audit", "--strict", "--formula", formula], "brew audit", env=env)
    report.record("brew audit --strict (the real release URLs, offline)", code == 0, brew_problems(out))
    code, out = run([brew, "style", formula], "brew style", env=env)
    report.record("brew style (the real release URLs)", code == 0, out.strip()[-600:])
    code, out = run([brew, "info", "--json=v2", "--formula", formula], "brew info", env=env)
    stable = formula_version(out) if code == 0 else None
    report.record(f"Homebrew reads the version {args.version} from the real release URLs", stable == args.version,
                  f"versions.stable {stable!r}")
    platform = {"Darwin": "macos", "Linux": "linux"}.get(host.system(), "?") + "-" + \
        ("arm64" if host.machine().lower() in ("arm64", "aarch64") else "x64")
    archive = helper_release.archive_name(args.version, platform)
    served = Served(args.release)
    try:
        loopback = manifests_for(args.release, args.version, served.url, args.work / "loopback")
        try:
            stated = loopback_formula((loopback / "homebrew" / "citar-helper.rb").read_text(encoding="utf-8"),
                                      args.version)
        except Failed as e:
            report.record("the loopback copy states the version", False, str(e))
            return report.write(args.out)
        target.write_text(stated, encoding="utf-8", newline="\n")
        before = len(served.requests)
        code, out = run([brew, "install", "--formula", formula], "brew install", env=env, timeout=1200)
        fetched = [r for r in served.requests[before:] if r.endswith(archive)]
        report.record(f"brew install (the loopback copy: {archive})", code == 0 and bool(fetched),
                      f"exit {code}, fetched {fetched}\n{out.strip()[-400:]}")
        code, out = run([brew, "test", formula], "brew test", env=env)
        report.record("brew test (--version and --self-test)", code == 0, out.strip()[-400:])
        code, out = run([brew, "--prefix"], "brew --prefix", env=env)
        prefix = Path(out.strip().splitlines()[-1]) if code == 0 and out.strip() else Path("/nonexistent")
        docs = prefix / "share" / "doc" / "citar-helper"
        notices = [n for n in ("LICENSE", "NOTICE.md", "THIRD-PARTY-LICENSES.txt") if (docs / n).is_file()]
        report.record("the notices are in its doc folder", len(notices) == 3, f"{docs}: {notices}")
        helper_runs(report, prefix / "bin" / "citar-helper", args.version, args.label, args.under,
                    args.work / "helper-home", "Homebrew's citar-helper")
        code, out = run([brew, "uninstall", "--formula", "citar-helper"], "brew uninstall", env=env)
        report.record("brew uninstall citar-helper", code == 0, out.strip()[-200:])
    finally:
        served.close()
        code, out = run([brew, "untap", "--force", tap], "brew untap", env=env)
        report.record(f"brew untap {tap}", code == 0, out.strip()[-200:])
    return report.write(args.out)


# ------------------------------------------------------------------------------------------------ command line


def main(argv: list[str] | None = None) -> int:
    top = argparse.ArgumentParser(prog="check_managers.py", description=__doc__.split("\n\n")[0],
                                  allow_abbrev=False)
    sub = top.add_subparsers(dest="command", required=True)
    p = sub.add_parser("structure", allow_abbrev=False)
    p.add_argument("--dir", type=Path, required=True, help="the manifests (release_checksums.py --out)")
    p.add_argument("--version", required=True)
    p.add_argument("--assets", type=Path, help="the release's files, whose hashes the manifests must carry")
    p.add_argument("--base-url", help="where the manifests point (default: the release's own downloads)")
    p = sub.add_parser("winget-schemas", allow_abbrev=False)
    p.add_argument("--dir", type=Path, required=True)
    p.add_argument("--schemas", type=Path, help="the schemas, by file name, instead of fetching them")
    for name in ("scoop", "winget", "brew"):
        p = sub.add_parser(name, allow_abbrev=False)
        p.add_argument("--release", type=Path, required=True, help="the release's files (pkg-release-assets)")
        p.add_argument("--version", required=True)
        p.add_argument("--label", help="the label the helper must name")
        p.add_argument("--work", type=Path, required=True)
        p.add_argument("--under", type=Path, default=os.environ.get("RUNNER_TEMP") or None,
                       help="gate_run.py helper's --under (default: $RUNNER_TEMP)")
        p.add_argument("--out", type=Path, required=True, help="the JSON report")
        if name == "winget":
            p.add_argument("--manifests", type=Path, required=True, help="the real-URL manifests (the preview's)")
    args = top.parse_args(argv)
    try:
        if args.command == "structure":
            version = release.Version.parse(args.version).text
            base = checksums.base_url(args.base_url, version)
            problems = structure(args.dir, version, base, args.assets)
            for problem in problems:
                print(f"check_managers: FAILED: {problem}", file=sys.stderr)
            if problems:
                return 1
            say(f"{len(MANIFESTS)} manifests of {version} hold, pointing at {base}"
                + (f", each hash its file's in {args.assets}" if args.assets else ""))
            return 0
        if args.command == "winget-schemas":
            problems, checked = winget_schemas(args.dir, args.schemas)
            for line in checked:
                say(f"validated: {line}")
            for problem in problems:
                print(f"check_managers: FAILED: {problem}", file=sys.stderr)
            return 1 if problems else 0
        release.Version.parse(args.version)
        args.release, args.work = args.release.resolve(), args.work.resolve()
        if args.under is None:
            raise Usage("--under (or $RUNNER_TEMP) is needed: the helper's home goes there")
        args.under = args.under.resolve()
        args.work.mkdir(parents=True, exist_ok=True)
        return {"scoop": cmd_scoop, "winget": cmd_winget, "brew": cmd_brew}[args.command](args)
    except (Usage, release.Refused) as e:
        print(f"check_managers: {e}", file=sys.stderr)
        return 2
    except checksums.Problem as e:
        print(f"check_managers: FAILED: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
