# Packaging

How each install route is built and published, and what a release needs for it to reach each one.

| Route | What it installs | Built and checked by | Published by |
|---|---|---|---|
| `pip install citar`, `pipx install citar` | the game, from the wheel for the machine (Windows x64, macOS 11+ on Apple silicon and Intel, Linux x64 and arm64 with glibc 2.28+; Python 3.11+); elsewhere pip builds the sdist with a Rust toolchain | `package.yml`: `wheels`, `sdist`, `sdist-build` | `release.yml`, PyPI trusted publishing |
| `curl … \| bash` | the game from its wheel ([`../install.sh`](../install.sh)); `--worker`, the self-updating helper | `package.yml`: `install` | the file on `main` |
| `& ([scriptblock]::Create((iwr …)))` | the same on Windows ([`../install.ps1`](../install.ps1)); `-Worker`, the helper | `package.yml`: `install` | the file on `main` |
| Windows installer and zip | the game, bundled ([`../installer/`](../installer/)): `CITAR-V-setup.exe`, per user, and the portable `CITAR-V-windows-x64.zip` | `package.yml`: `windows` | `release.yml`, on the GitHub release |
| The server image | `ghcr.io/jprodgers/citar:V`, linux/amd64 and linux/arm64 ([`../Dockerfile`](../Dockerfile)) | `package.yml`: `image` | `release.yml` (GHCR; `GITHUB_TOKEN` is enough) |
| The CITAR helper | five self-updating binaries under names that never change, and their manifest | `package.yml`: `helpers`, `helper-manifest` | `release.yml`, on the GitHub release ([below](#the-citar-helper)) |
| Homebrew | `citar-helper`, the packaged helper (macOS arm64 and x64, Linux x64 and arm64) | `package.yml`: `managers` | you, once per release: the tap [jprodgers/homebrew-citar](https://github.com/jprodgers/homebrew-citar) |
| Scoop | `citar` (the game's zip) and `citar-helper` (the packaged helper) | `package.yml`: `managers` | you, once per release: the bucket [jprodgers/scoop-citar](https://github.com/jprodgers/scoop-citar) |
| winget | `JimmieRodgers.CITAR` (the game's installer) and `JimmieRodgers.CITAR.Helper` (the packaged helper) | `package.yml`: `managers` | you, once per release: a pull request to `microsoft/winget-pkgs` |

Everything a release ships is built, installed and tested on every pull request by `.github/workflows/package.yml`
(design/phase5-release.md P5.3 to P5.10); `release.yml` builds nothing, and publishes what `package.yml` assembled. The
last three routes need a repository outside this one, or Microsoft's review, which is why their manifests are written
here as files to copy rather than pushed by a workflow.

## The order things happen in

1. **`package.yml`** builds every file of the release, each on its own platform, and tests each one where it was
   built. Its `assemble` job then downloads them by name into one folder, which must be exactly the release's list
   (`python scripts/release.py assets V`), and holds every build to one: one version, one label, one build id and
   one jobs build across the wheels, the helpers, the bundle, both images and the install scripts' runs, and one
   result hash for the seeded lab job every wheel played (`scripts/ci/assemble_release.py`). It writes `SHA256SUMS`,
   the release notes, the attestation's subject list and the package managers' manifests, and uploads three
   artifacts: `pkg-release-assets` (every file), `pkg-python-dists` (the wheels and the sdist) and
   `pkg-release-preview` (the notes, the sums, the subjects, the asset list with sizes, the one build, the
   manifests). Its `managers` job installs each package manager's packages from a loopback copy of that folder.
2. **`release.yml`**, on a tag, signs the helper manifest, attests every file, publishes PyPI and the image, then the
   GitHub release with every file, published last and as a whole (design/phase5-release.md P5.11).
3. **Homebrew, Scoop, winget**: their manifests carry the SHA-256 of files that exist only once step 2 is done.
   Copy them from the `pkg-release-preview` artifact, or write them again from the published release (below).

## The manifests: templates, and `release_checksums.py`

Each manifest is a template here, `packaging/**/*.in`, filled in from a release's files by
[`scripts/release_checksums.py`](../scripts/release_checksums.py) in this vocabulary: `{{version}}`, `{{date}}` (the
date of the version's CHANGELOG section), `{{url:NAME}}`, `{{sha256:NAME}}` and `{{SHA256:NAME}}` (upper case, as
winget writes it), where `NAME` is one of the release's files. A name that is not one of them, or a placeholder it
does not know, is refused with the template's line, so no manifest can point at a file a release does not have. None
names PyPI: the game's routes install from the release's own files.

| Template | Becomes | Copied to |
|---|---|---|
| `homebrew/citar-helper.rb.in` | `homebrew/citar-helper.rb` | the tap's `Formula/citar-helper.rb` |
| `scoop/citar.json.in`, `scoop/citar-helper.json.in` | `scoop/citar.json`, `scoop/citar-helper.json` | the bucket's `bucket/` |
| `winget/game/*.yaml.in` | `winget/game/`, three files | `wingetcreate submit` |
| `winget/helper/*.yaml.in` | `winget/helper/`, three files | `wingetcreate submit` |

```bash
python scripts/release_checksums.py 0.1.6 --write                       # from the published release's SHA256SUMS
python scripts/release_checksums.py 0.1.6 --from-dir release --out m    # from built files (CI's assemble job)
python scripts/release_checksums.py 0.1.6 --from-dir release --base-url http://127.0.0.1:8123/ --out m
python scripts/release_checksums.py --verify 0.1.6                      # the published release and PyPI's files
```

- `--write` writes the manifests beside their templates for review; git ignores them (one release's numbers, never
  source). `--out DIR` writes them elsewhere, in this folder's layout.
- `--from-dir DIR` hashes the files in DIR; without it the hashes come from the published release's `SHA256SUMS`.
- `--base-url` points the manifests at another copy of the release (https, or http to a loopback address only):
  the `managers` job's loopback server.
- `--verify V` (or `V --verify`, as the release checklist writes it; the two must not name different versions)
  downloads the release's `SHA256SUMS`, which must list exactly the release's files, then every file and
  its `.sha256`, each held to the sums, and checks that PyPI holds exactly the release's wheels and sdist with the same
  hashes. Every problem names its file. Its dry-run form, `--base-url http://127.0.0.1:<port>` and `--pypi-json FILE`
  (`--from-dir DIR --write-pypi-json FILE` writes one from the built wheels and sdist), is rehearsed before a release.

## Homebrew

A personal tap, not homebrew-core, holding one formula: **`citar-helper`**, the helper's packaged build, for macOS on
Apple silicon and Intel and Linux on x64 and arm64 (one archive and SHA-256 under each of the formula's four
`on_macos`/`on_linux` × `on_arm`/`on_intel` blocks). It installs `citar-helper` and its notices (`LICENSE`,
`NOTICE.md`, `THIRD-PARTY-LICENSES.txt`, into the formula's `doc`), and its `test` runs `--version` and
`--self-test`. It states its `version`: from the archives' names alone Homebrew reads "64" (`...-arm64.tar.gz`). The
game itself installs from its wheels (`pipx install citar`, or `install.sh`), so 0.1.5's `citar`
formula, which built the sdist, is gone; the tap's `Formula/citar.rb` is removed when `citar-helper.rb` is added, and
`brew uninstall citar` removes an old copy.

A packaged build never updates itself (`brew upgrade citar-helper` does), and its service runs through Homebrew:

```bash
brew install jprodgers/citar/citar-helper
citar-helper setup                      # the server's address and a token
brew services start citar-helper        # runs `citar-helper run --service` at login, through opt_bin
```

`brew services` points at Homebrew's `opt` path, which survives upgrades; `citar-helper service install` would
record this version's Cellar path, which the next `brew upgrade` removes. The formula's caveats say so.

**Checked on every pull request** (`package.yml`'s `managers`, on macOS arm64, macOS x64 and Linux x64): `brew audit
--strict` and `brew style` on the formula with the release's real URLs (offline: the files are not published yet),
then `brew install` and `brew test` on a copy pointed at a loopback server holding the release, each in a tap of its
own (`brew tap-new --no-git citar/dryrun`). Linux arm64's block is held by `scripts/ci/check_managers.py structure`.

## Scoop

A bucket is a repository with a `bucket/` folder of JSON manifests:
[jprodgers/scoop-citar](https://github.com/jprodgers/scoop-citar). It holds two:

- **`citar`**: the game's portable zip, `CITAR-V-windows-x64.zip` (its `CITAR/` folder; `citar` and `citar-play` on
  PATH, a Start menu shortcut);
- **`citar-helper`**: the helper's packaged build, `citar-helper-V-windows-x64.zip` (`citar-helper` on PATH), which
  Scoop updates (`scoop update citar-helper`) and which never updates itself.

```bash
scoop bucket add citar https://github.com/jprodgers/scoop-citar
scoop install citar citar-helper
```

Each carries `checkver` and `autoupdate` blocks, so Scoop's own tooling can raise the version from a new GitHub
release (each file's `.sha256` beside it is its hash). **Checked on every pull request** (`managers` on Windows):
Scoop installed by its installer (pinned to a commit and its SHA-256, with `-RunAsAdmin`, since a runner's shell is
elevated), then `scoop install` of both manifests from a loopback copy of the release, `citar --version`,
`citar-helper --version`, and `scoop uninstall`.

## winget

winget's manifests live in Microsoft's repository, and a submission is a pull request that a validation pipeline
checks. Two packages, each three files (version, installer, default locale):

- **`JimmieRodgers.CITAR`**: the game's Inno Setup installer, per user (no elevation);
- **`JimmieRodgers.CITAR.Helper`**: the helper's packaged build in its zip, installed as a portable command
  (`NestedInstallerType: portable`, the command alias `citar-helper`), per user; winget upgrades it. A portable
  takes no `Scope` (winget validate warns on one).

On a Windows machine, once:

```powershell
winget install Microsoft.WingetCreate
wingetcreate token --store
```

`token --store` opens a GitHub sign-in. It needs a **classic** personal access token with the `public_repo` scope
(fine-grained tokens are not supported), and it creates the fork of `microsoft/winget-pkgs` that the pull request
comes from. Then, per release, for each package:

```powershell
winget validate --manifest packaging\winget\game
wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR 0.1.6" packaging\winget\game
winget validate --manifest packaging\winget\helper
wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR.Helper 0.1.6" packaging\winget\helper
```

Validate every time. `wingetcreate submit` validates too, but when it fails it prints `ERROR: Path does not exist`
underneath the real errors, which sends you looking at the path instead of at the manifests. Two things only winget's
own validator catches, which `scripts/ci/check_managers.py structure` now checks as well:

* **The schema header.** Every file must *begin* with its
  `# yaml-language-server: $schema=https://aka.ms/winget-manifest.<type>.<version>.schema.json` line. Without it the
  whole set is rejected with "Schema header not found", however correct the contents are.
* **Field names.** The published JSON schemas do not set `additionalProperties: false`, so a misspelled or renamed
  field validates against them and is silently dropped. It is `Documentations`, not `Documentation`.

`submit` is the command for a package's first version; `wingetcreate update <id>` is for later ones, and fails until
the package is in the repository. The first submission is reviewed by a person and can take days; later ones are
usually automatic. A package identifier is permanent once accepted. `ManifestVersion` is the schema the files are
written against (1.12.0, what current submissions use), not CITAR's version: check what new submissions in
`winget-pkgs` use before a release, and move the templates' headers and `ManifestVersion` together.

**Checked on every pull request** (`managers` on Windows): every file against the JSON schema its header names
(`jsonschema`; the gate), then `winget validate` and `winget install --manifest` from a loopback copy of the release
where the runner has winget, and a notice saying they were not run where it has not. A winget older than the
files' `ManifestVersion` (windows-latest's 1.11 against 1.12.0) warns on every schema header, holding it to its own
newest schema; that warning alone passes, with a notice, and any other warning fails.

## The CITAR helper

The helper (`crates/citar-helper`, design/phase4-helper.md P4.10) is one Rust binary per target, built twice on each
target's own runner by `package.yml`'s `helpers` job (the Linux ones in the manylinux_2_28 containers, so they need
glibc 2.28 at most, as the game does):

| Target | Runner | Self-updating (release asset) | Packaged (Homebrew, Scoop, winget) |
|---|---|---|---|
| `x86_64-pc-windows-msvc` | `windows-latest` | `citar-helper-windows-x64.exe` | `citar-helper-V-windows-x64.zip` |
| `aarch64-apple-darwin` | `macos-15` | `citar-helper-macos-arm64` | `citar-helper-V-macos-arm64.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | `citar-helper-macos-x64` | `citar-helper-V-macos-x64.tar.gz` |
| `x86_64-unknown-linux-gnu` | `ubuntu-24.04` | `citar-helper-linux-x64` | `citar-helper-V-linux-x64.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `citar-helper-linux-arm64` | `citar-helper-V-linux-arm64.tar.gz` |

- **The self-updating builds** keep names that never change, so the server's download links
  (`releases/latest/download/<name>`) always fetch the newest; `install.sh --worker` and `install.ps1 -Worker`
  download them with their `.sha256`. They update themselves from the signed `citar-helper-manifest.json`, which
  `package.yml`'s `helper-manifest` job writes over the five and signs there only with a throwaway key, to prove the
  path. The real signature is a tag-only job of `release.yml`'s, in the protected `helper-signing` environment, with
  the owner's key (design/phase5-release.md P5.4.2 and P5.11); until the public key is in
  `crates/citar-jobs/update-keys.pub`, every helper reports updates unavailable. The owner's one-time setup is
  [WORKERS.md, "Signing helper updates"](../docs/server/WORKERS.md#signing-helper-updates-the-owners-one-time-setup).
- **The packaged builds** (`--features packaged`) are in versioned archives with the notices beside the binary:
  `updates` defaults to `notify` and `auto` is refused ("update it through your package manager"), so the helper
  never fights its package manager over its own binary.
- `citar/web/tests/helper.test.mjs` holds the five names equal in the download card, `package.yml`'s `helpers`
  matrix, `helper.yml`'s artifact matrix and `cargo xtask helper-manifest`'s table.
- The macOS binaries are not notarised, so a download needs `xattr -d com.apple.quarantine`; Homebrew's install does
  not.

## Code signing

None of the Windows files are signed, because a certificate costs money every year and a personal one now needs a
hardware token. What that means in practice:

- SmartScreen shows "Windows protected your PC" on the installer until enough people have run it.
- Some antivirus products flag PyInstaller output on sight, regardless of contents.

The honest mitigations, all of which a release does: the SHA-256 of every file (`SHA256SUMS`, and a `.sha256` beside
each), build provenance for every file (`gh attestation verify <file> --repo jprodgers/CITAR`), every file built in
a public CI run whose log anyone can read, and the README telling people plainly what they will see. If CITAR ever
gets a certificate, the signing step goes in `package.yml`'s `windows` job, between the bundle and the installer.
