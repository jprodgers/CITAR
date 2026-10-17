# Packaging

How each install route is published, and what has to happen for a release to reach it.

| Route | Lives in | Published by | Needs |
|---|---|---|---|
| `pip install citar` | [`../pyproject.toml`](../pyproject.toml) | `.github/workflows/release.yml` | PyPI trusted publishing |
| `curl … \| bash` | [`../install.sh`](../install.sh) | the file on `main` | nothing — it installs from PyPI |
| `iwr … \| iex` | [`../install.ps1`](../install.ps1) | the file on `main` | nothing |
| Windows installer | [`../installer/`](../installer/) | release workflow | a Windows runner |
| Docker image | [`../Dockerfile`](../Dockerfile) | release workflow | GHCR (no secret; `GITHUB_TOKEN` is enough) |
| Homebrew | [`homebrew/citar.rb`](homebrew/citar.rb) | you, once per release | a `homebrew-citar` tap repository |
| Scoop | [`scoop/citar.json`](scoop/citar.json) | you, once per release | a `scoop-citar` bucket repository |
| winget | [`winget/`](winget/) | you, once per release | a pull request to `microsoft/winget-pkgs` |
| The CITAR helper's five binaries, and their signed manifest | `crates/citar-helper` | release workflow (`build-helper`, `sign-helper-manifest`) | the `helper-signing` environment's key, for the manifest ([below](#the-citar-helper)) |

The first five are automatic on a tag. The last three need a repository or a pull request outside
this one, which is why they are checked in here as files you copy rather than as workflow steps
that would fail on a fork.

## The order things have to happen in

A release is not a single event. Each of these depends on the one before it:

1. **Tag and push.** `release.yml` builds the wheel, the installer and the image.
2. **PyPI.** Everything else installs *from* PyPI, so nothing below works until this has landed.
3. **GitHub release.** The installer `.exe` and its checksum are attached here; Scoop and winget
   point at those URLs.
4. **Homebrew, Scoop, winget.** Each needs the SHA-256 of a file that did not exist until step 3.

`scripts/release_checksums.py` prints the hashes and the edited manifests once the release assets
are up, so this is copy and paste rather than arithmetic.

## Homebrew

A personal tap, not homebrew-core. Core requires every Python dependency to be listed as a pinned
`resource` block, which for CITAR is around forty of them, regenerated on every dependency change —
worth doing if CITAR is ever submitted to core, and not worth doing before anyone has asked for it.

The tap exists: [jprodgers/homebrew-citar](https://github.com/jprodgers/homebrew-citar). Each
release, after `release_checksums.py` has filled in the version and hash:

```bash
cp packaging/homebrew/citar.rb ../homebrew-citar/Formula/citar.rb
cd ../homebrew-citar && git commit -am "CITAR X.Y.Z" && git push
```

Then `brew install jprodgers/citar/citar`.

## Scoop

A bucket is a repository with a `bucket/` directory of JSON manifests. The manifest points at the
GitHub release asset, so it needs the release to exist first.

The bucket exists: [jprodgers/scoop-citar](https://github.com/jprodgers/scoop-citar).

```bash
cp packaging/scoop/citar.json ../scoop-citar/bucket/citar.json
cd ../scoop-citar && git commit -am "CITAR X.Y.Z" && git push
```

The manifest carries `checkver` and `autoupdate` blocks, so Scoop's own tooling can raise the
version from a new GitHub release without either file being edited by hand.

Then `scoop bucket add citar https://github.com/jprodgers/scoop-citar && scoop install citar`.

## winget

winget manifests live in Microsoft's own repository, and a submission is a pull request that a
validation pipeline checks. The three files here are the complete manifest set for one version, so
`wingetcreate` has nothing to generate and only has to submit them.

On a Windows machine, once:

```powershell
winget install Microsoft.WingetCreate
wingetcreate token --store
```

`token --store` opens a GitHub sign-in. It needs a **classic** personal access token with the
`public_repo` scope - fine-grained tokens are not supported - and it creates the fork of
`microsoft/winget-pkgs` that the pull request comes from.

Validate before submitting, every time:

```powershell
winget validate --manifest packaging\winget
```

This is not optional politeness. `wingetcreate submit` validates too, but when it fails it
prints `ERROR: Path does not exist` underneath the real errors, which sends you looking at the
path instead of at the manifests. `winget validate` says what is actually wrong.

Two things it catches that nothing else does:

* **The schema header.** Every file must *begin* with its
  `# yaml-language-server: $schema=https://aka.ms/winget-manifest.<type>.<version>.schema.json`
  line. Without it the whole set is rejected with "Schema header not found", however correct
  the contents are.
* **Field names.** The published JSON schemas do not set `additionalProperties: false`, so a
  misspelled or renamed field validates happily against them and is silently dropped. It was
  `Documentations`, not `Documentation`, and only winget's own validator said so.

Then, per release:

```powershell
wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR 0.1.0" packaging\winget
```

`submit` is the right command for a first version. `wingetcreate update JimmieRodgers.CITAR` is for
later ones and only works once the package is already in the repository - used too early it fails,
because there is nothing there to update.

The first submission is reviewed by a person and can take a few days; later versions are usually
automatic. The package identifier is permanent once accepted, so `JimmieRodgers.CITAR` is a
decision rather than a placeholder.

`ManifestVersion` is the schema version the files are written against, not CITAR's version. Check
what current submissions in `winget-pkgs` use before a release and match it: an old schema is
accepted for a while, and then one day is not.

## The CITAR helper

The helper (`crates/citar-helper`, design/phase4-helper.md P4.10) is one Rust binary per target, attached to every
release under five names that never change, so the server's download links (`releases/latest/download/<name>`)
always fetch the newest:

| Target | Runner | Asset |
|---|---|---|
| `x86_64-pc-windows-msvc` | `windows-latest` | `citar-helper-windows-x64.exe` |
| `aarch64-apple-darwin` | `macos-15` | `citar-helper-macos-arm64` |
| `x86_64-apple-darwin` | `macos-15-intel` | `citar-helper-macos-x64` |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `citar-helper-linux-x64` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | `citar-helper-linux-arm64` |

`release.yml`'s `build-helper` builds them (`cargo build --release --locked -p citar-helper --target ...`, no
features), runs each once with `--version` (which must name its target) and `--self-test`, and writes each one's
`.sha256` beside it. The Linux builds are glibc-based on Ubuntu 22.04, so they run on older distributions, and TLS uses
the OS's own trust store (checked on Fedora in the job). `citar/web/tests/helper.test.mjs` holds the five names equal
in the download card (`citar/web/js/helperrules.js`), this matrix, `helper.yml`'s artifact matrix and `cargo xtask
helper-manifest`'s table. `helper.yml` builds the same five on every change to the helper, runs its tests there, and
installs the builds as services on the runners (the Windows service, the Run value, a systemd user unit, the
LaunchAgent).

Then `sign-helper-manifest` writes `citar-helper-manifest.json` over the five (`cargo xtask helper-manifest`) and signs
it with the key the protected `helper-signing` environment holds (`cargo xtask sign-manifest --key-env
CITAR_UPDATE_SIGNING_KEY`): the manifest and its `.sig` are attached beside the binaries, and helpers update themselves
from them. Without the secret the job signs nothing and says so; the owner's one-time setup is
[WORKERS.md, "Signing helper updates"](../docs/server/WORKERS.md#signing-helper-updates-the-owners-one-time-setup).

**What Phase 5 does for the helper** (the release workflow's helper jobs stay behind its "Release packaging is Phase
5" gate until then):

- **The gate removed**, and a release dry run (`workflow_dispatch`) before the first tag that ships the helper.
- **The key:** the owner's public key in `crates/citar-jobs/update-keys.pub` (one line, two slots for rotation) in a
  release commit, and the secret in `helper-signing`; until then every helper reports updates unavailable.
- **The Homebrew tap ships only the helper** (R4 of the plan: the prebuilt binaries for macOS arm64 and x64 and Linux,
  not the whole game), and Scoop and winget gain a helper package. Each of these is built with the `packaged`
  feature: `updates` defaults to `notify` and `auto` is refused ("update through your package manager"), so the helper
  never fights the package manager over its own binary. The GitHub release assets are built without it.
- **Install scripts:** `install.sh --worker` and `install.ps1 -Worker`, which only say where to download the helper
  today, download it.
- **Attestations** of the built binaries, and code signing and notarisation if ever (below): the macOS binaries are
  not notarised, so a download needs `xattr -d com.apple.quarantine`.

## Code signing

None of the Windows artifacts are signed, because a certificate costs money every year and a
personal one now needs a hardware token. What that means in practice:

- SmartScreen shows "Windows protected your PC" on the installer until enough people have run it.
- Some antivirus products flag PyInstaller output on sight, regardless of contents.

The honest mitigations, all of which this release does: publish the SHA-256 of every asset, build
the artifacts in a public CI run whose log anyone can read, and tell people plainly in the README
what they will see. If CITAR ever gets a certificate, the signing step goes in `release.yml` between
the build and the upload.
