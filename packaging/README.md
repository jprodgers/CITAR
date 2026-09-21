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

## Code signing

None of the Windows artifacts are signed, because a certificate costs money every year and a
personal one now needs a hardware token. What that means in practice:

- SmartScreen shows "Windows protected your PC" on the installer until enough people have run it.
- Some antivirus products flag PyInstaller output on sight, regardless of contents.

The honest mitigations, all of which this release does: publish the SHA-256 of every asset, build
the artifacts in a public CI run whose log anyone can read, and tell people plainly in the README
what they will see. If CITAR ever gets a certificate, the signing step goes in `release.yml` between
the build and the upload.
