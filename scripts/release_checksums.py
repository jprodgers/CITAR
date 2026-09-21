"""Print the checksums a release needs, and the manifest lines that carry them.

Homebrew, Scoop and winget each pin the hash of a file that does not exist until the GitHub release
is published, so updating them is the last step of a release and the one most likely to be done by
hand and got wrong. This turns it into copy and paste.

    python scripts/release_checksums.py 0.1.0            # from the published release
    python scripts/release_checksums.py 0.1.0 --local    # from files built in this checkout

With ``--write`` it edits the manifests in ``packaging/`` in place, so the diff can be reviewed
before anything is copied to a tap or a bucket.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "jprodgers/CITAR"

#: label -> (URL template, local path template). Every asset a manifest refers to.
ASSETS = {
    "sdist": (
        "https://files.pythonhosted.org/packages/source/c/citar/citar-{version}.tar.gz",
        "dist/citar-{version}.tar.gz",
    ),
    "windows-installer": (
        f"https://github.com/{REPO}/releases/download/v{{version}}/CITAR-{{version}}-setup.exe",
        "installer/output/CITAR-{version}-setup.exe",
    ),
    "windows-zip": (
        f"https://github.com/{REPO}/releases/download/v{{version}}/CITAR-{{version}}-windows-x64.zip",
        "dist/CITAR-{version}-windows-x64.zip",
    ),
}


def sha256_of_url(url: str) -> str:
    """Hash a published asset without keeping the whole file in memory."""
    digest = hashlib.sha256()
    with urllib.request.urlopen(url) as response:
        for chunk in iter(lambda: response.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_of_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def collect(version: str, local: bool) -> dict[str, str]:
    """The hash of every asset, or a note saying why it is missing."""
    hashes: dict[str, str] = {}
    for label, (url_template, path_template) in ASSETS.items():
        try:
            if local:
                path = ROOT / path_template.format(version=version)
                if not path.exists():
                    print(f"  {label}: not built ({path.relative_to(ROOT)})", file=sys.stderr)
                    continue
                hashes[label] = sha256_of_file(path)
            else:
                hashes[label] = sha256_of_url(url_template.format(version=version))
        except Exception as exc:
            print(f"  {label}: {type(exc).__name__}: {exc}", file=sys.stderr)
    return hashes


def update_manifests(version: str, hashes: dict[str, str]) -> list[str]:
    """Rewrite the version and hash in each packaging manifest. Returns what changed."""
    changed = []

    formula = ROOT / "packaging" / "homebrew" / "citar.rb"
    if "sdist" in hashes and formula.exists():
        text = formula.read_text(encoding="utf-8")
        text = re.sub(r"citar-[\d.]+\.tar\.gz", f"citar-{version}.tar.gz", text)
        text = re.sub(r'sha256 "[0-9a-f]{64}"', f'sha256 "{hashes["sdist"]}"', text)
        formula.write_text(text, encoding="utf-8", newline="\n")
        changed.append(str(formula.relative_to(ROOT)))

    manifest = ROOT / "packaging" / "scoop" / "citar.json"
    if "windows-zip" in hashes and manifest.exists():
        data = json.loads(manifest.read_text(encoding="utf-8"))
        data["version"] = version
        arch = data["architecture"]["64bit"]
        arch["url"] = ASSETS["windows-zip"][0].format(version=version)
        arch["hash"] = hashes["windows-zip"]
        manifest.write_text(json.dumps(data, indent=4) + "\n", encoding="utf-8", newline="\n")
        changed.append(str(manifest.relative_to(ROOT)))

    winget = ROOT / "packaging" / "winget"
    if "windows-installer" in hashes and winget.is_dir():
        for path in sorted(winget.glob("*.yaml")):
            text = path.read_text(encoding="utf-8")
            text = re.sub(r"^PackageVersion: .*$", f"PackageVersion: {version}", text,
                          flags=re.MULTILINE)
            text = re.sub(r"CITAR-[\d.]+-setup\.exe", f"CITAR-{version}-setup.exe", text)
            text = re.sub(r"/download/v[\d.]+/", f"/download/v{version}/", text)
            text = re.sub(r"tag/v[\d.]+", f"tag/v{version}", text)
            text = re.sub(r"InstallerSha256: [0-9a-fA-F]{64}",
                          f"InstallerSha256: {hashes['windows-installer'].upper()}", text)
            path.write_text(text, encoding="utf-8", newline="\n")
            changed.append(str(path.relative_to(ROOT)))

    return changed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version", help="the release version, without a leading v")
    parser.add_argument("--local", action="store_true",
                        help="hash the files built in this checkout instead of the published ones")
    parser.add_argument("--write", action="store_true",
                        help="update the manifests in packaging/ in place")
    args = parser.parse_args()

    version = args.version.lstrip("v")
    print(f"CITAR {version} — checksums\n")
    hashes = collect(version, args.local)
    if not hashes:
        print("\nNothing to hash. Publish the release first, or pass --local after building.")
        return 1

    width = max(len(label) for label in hashes)
    for label, digest in hashes.items():
        print(f"  {label:<{width}}  {digest}")

    if args.write:
        print()
        for path in update_manifests(version, hashes):
            print(f"  updated {path}")
        print("\nReview the diff, then copy each manifest to its tap, bucket or pull request.")
        print("packaging/README.md has the commands.")
    else:
        print("\nRe-run with --write to put these into packaging/.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
