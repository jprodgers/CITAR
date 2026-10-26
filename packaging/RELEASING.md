# Releasing CITAR

The checklist for cutting a release, each step a command (design/phase5-release.md P5.15). It is written for 0.1.6;
for another version change the number everywhere. Everything here is the maintainer's: no workflow pushes a tag,
publishes to PyPI or the registry, or updates the tap, the bucket or winget on its own, and an agent working on the
repository never does any of it.

How the pieces fit: `.github/workflows/package.yml` builds and tests every file a release ships on every pull request;
`.github/workflows/release.yml` builds nothing, and on a pushed `v*` tag signs, attests and publishes what
`package.yml` assembled, in an order that keeps everything private until the signature and the provenance are made
(below). A dispatch of `release.yml` is a dry run that publishes nothing. [README.md](README.md) in this folder
describes each install route.

## 1. Before

- Phase 6's soak, QA and security review are done, and the owner has said go.
- The version number is decided (0.1.6).
- **Helper updates.** A helper trusts only the update keys it was built with, so if a release ships with
  `crates/citar-jobs/update-keys.pub` empty, no helper installed from it can ever update itself (each would be
  replaced by hand at the next release). For updates to work from this release on, before the tag (from the
  repository's root; the commands are bash's, Git Bash's on Windows, and the `cargo xtask` lines work as written in
  PowerShell too):
  1. Make the key on a machine of your own, in a folder outside every git repository (the tool refuses one inside):
     `cargo xtask keygen --out "$HOME/citar-update-key"` (in cmd, `%USERPROFILE%\citar-update-key`). Write the
     folder as a full path: Windows PowerShell 5.1 passes a `~` to a program as it is, so `~/citar-update-key` would
     name a folder inside the repository, which the tool refuses. It writes `citar-update-signing.key` (the private
     key, readable by you alone) and `citar-update-signing.pub` (one line, the public key). Keep a copy of the
     private file offline too: it is the only way to sign updates that helpers already trust.
  2. On GitHub, the repository's **Settings → Environments → New environment**, named exactly `helper-signing`,
     **before the first tag**: a tag's run names it, and GitHub makes a missing environment by itself, with no
     protection at all. (If one was made that way, give it the two rules below before it gets the secret.)
     - Under "Deployment protection rules" tick **Required reviewers** and add yourself, so every signing run waits
       for your approval.
     - Under "Deployment branches and tags" choose **Selected branches and tags** and add a rule of type **Tag** with
       the pattern `v*`, so only a tag's run can reach the secret, whatever a branch's workflow says.
     - Under "Environment secrets" add `CITAR_UPDATE_SIGNING_KEY`, whose value is the whole content of
       `citar-update-signing.key`.
  3. Commit the line of `citar-update-signing.pub` to `crates/citar-jobs/update-keys.pub` (one key a line, at most
     two) in the release's bump pull request (step 2), so the release's helpers are built trusting it.
  4. Check: `gh api repos/jprodgers/CITAR/environments --jq '[.environments[].name]'` lists `helper-signing`
     (beside `github-pages` and `pypi`). Then sign and verify a copy of the test manifest in a scratch folder outside
     the repository, never the one in `tests/data/helper_manifest/` (signing writes `MANIFEST.sig` beside it, and
     that folder's `.sig` is a fixture the tests hold byte for byte):

     ```bash
     mkdir -p "$HOME/citar-sign-check"
     cp tests/data/helper_manifest/citar-helper-manifest.json "$HOME/citar-sign-check/"
     cargo xtask sign-manifest --key "$HOME/citar-update-key/citar-update-signing.key" "$HOME/citar-sign-check/citar-helper-manifest.json"
     cargo xtask verify-manifest --pub crates/citar-jobs/update-keys.pub "$HOME/citar-sign-check/citar-helper-manifest.json"
     git status --short                                   # nothing new in the repository
     ```

     `verify-manifest` reads the `.sig` beside the manifest and names the line of `update-keys.pub` that verified it.
     Delete the scratch folder afterwards.

  The release checks the two halves against each other (`scripts/ci/release_sign.py`): an empty key file and no
  secret gives a notice and an unsigned release (its helpers report updates unavailable); a secret with no committed
  key, a committed key with no secret, or a secret that is not the committed key stops the release before anything is
  public. [WORKERS.md, "Signing helper updates"](../docs/server/WORKERS.md#signing-helper-updates-the-owners-one-time-setup)
  has the background and how to rotate the key.

## 2. Bump

On a branch from `main` (once the 0.1.6 work is merged there):

```bash
git switch -c release-0.1.6 origin/main
python scripts/release.py bump 0.1.6 --date YYYY-MM-DD    # the release's date; it prints each place it moves
git status --short                # only the files bump printed (and update-keys.pub, from step 1.3)
git diff                                                  # every place moves, nothing else
python scripts/release.py check --version 0.1.6
python scripts/check_links.py --strict
git add -u -- . ':!.claude'       # the tracked files bump changed, never an editor's or a local setting
git status --short                # the same list, now staged
git commit -m "Release 0.1.6"
git push -u origin release-0.1.6
gh pr create --base main --title "Release 0.1.6" --fill
```

Never `git commit -a` or `git add -A` here: a checkout with local changes of its own (an editor's settings,
`.claude/launch.json`) would put them in the release. If `git status --short` lists anything that is not the
release's, leave it out of the `git add` (or name the files one by one).

`bump` refuses a version that is not newer, rewrites the version in each place (`Cargo.toml`, both lock files, the
package, the installer script, `KNOWN_ISSUES.md`) and turns the changelog's Unreleased section into `## [0.1.6] -
date` under a new empty one, with both compare links. Every workflow must be green on the pull request (`gh pr
checks`): `tests`, `package` (every file of the release, built and tested), `rust`, `helper` and `determinism`.

## 3. Dry run

A dispatch of `release.yml` on the branch: the whole release built and tested as a tag would build it, every tag-only
job skipped, nothing published, no environment named.

```bash
gh workflow run release.yml --ref release-0.1.6 -f version=0.1.6
gh run list --workflow release.yml --limit 1          # its id
gh run watch <id> --exit-status
```

`meta`, `tests`, `package` and `dry-run-summary` must be green and every other job skipped. The run's summary page
(`dry-run-summary`) shows the version and label (`v0.1.6-dryrun+<sha>`), the one build, what each tag-only job would
do and with which environment, every asset with its size and SHA-256, the attestation's subjects, both images loaded
with their architectures, and the tag-only jobs' file steps rehearsed on the run's files. Its preview holds the
release notes and the package managers' manifests:

```bash
gh run download <id> -n pkg-release-preview -D preview-0.1.6
python scripts/release.py assets 0.1.6 > assets.txt && diff assets.txt preview-0.1.6/subjects.txt
```

## 4. Merge

Merge the pull request to `main` and wait for `main`'s runs to be green: `gh run list --branch main --limit 6`.

## 5. Tag

The merge commit, annotated:

```bash
git switch main && git pull --ff-only
git log -1 --oneline                      # the release's merge commit
git tag -a v0.1.6 -m "CITAR 0.1.6"
git push origin v0.1.6
```

## 6. Watch the release run

```bash
gh run list --workflow release.yml --event push --limit 1
gh run watch <id>
```

When `sign-helper-manifest` waits (only when the `helper-signing` environment exists), approve it: the run's page,
**Review deployments**, tick `helper-signing`, **Approve and deploy**. The jobs run in this order:

| Job | What it does on the tag | Holds |
|---|---|---|
| `meta` | the version from the tag; `release.py prepare --mode tag` (the tag, the source and the changelog agree) | nothing |
| `tests`, `package` | test.yml and package.yml, every test and every file of the release | nothing |
| `sign-helper-manifest` | waits for its reviewer, then signs `citar-helper-manifest.json` with the prebuilt xtask, or gives the notice that nothing is signed | the secret |
| `check-signature` | the signature checked against the committed `update-keys.pub` by a verifier that shares no code with the signer; `SHA256SUMS` and the notes written again with it | nothing |
| `attest-files` | build provenance for every file and the signature | `id-token`, `attestations` |
| `publish-pypi` | the five wheels and the sdist to PyPI (trusted publishing, `skip-existing`) | PyPI |
| `publish-image` | `ghcr.io/jprodgers/citar:0.1.6-amd64` and `-arm64` pushed (the tested images), joined as `:0.1.6` | `packages` |
| `attest-image` | provenance for `:0.1.6`'s digest, pushed to the registry | `packages`, `id-token`, `attestations` |
| `github-release` | a draft with every file and the notes, its list held to `release.py assets 0.1.6`, then published, latest unless a prerelease | `contents` |
| `image-tags` | `:0.1` and `:latest` moved to `:0.1.6`'s digest (a prerelease moves neither) | `packages` |

Until `attest-files` has passed nothing is public. After it, what is public grows job by job, and a failure is
finished by re-running, never by deleting what was published:

| Failed job | What is public | What to do |
|---|---|---|
| `meta`, `tests`, `package` | nothing | Fix on a branch from `main`, merge, then delete the tag and tag again (below). A flaky test: `gh run rerun <id> --failed` |
| `sign-helper-manifest` refused | nothing | The message names the cause. A secret that is wrong or missing: fix the secret in the environment, then `gh run rerun <id> --failed`. A key file that is wrong: fix it on a branch, merge, delete the tag and tag again |
| `sign-helper-manifest` waiting | nothing | Approve it (above), or reject it to stop the release |
| `check-signature` | nothing | As for the signing job: it fails when the signature does not match the committed key, or a signature came with no key committed |
| `attest-files` | provenance for some files, in the public log (harmless alone) | `gh run rerun <id> --failed` |
| `publish-pypi` | some or all of the files on PyPI | `gh run rerun <id> --failed`: `skip-existing` uploads only what is missing. Never delete a file from PyPI: its name can never be used again |
| `publish-image` | some of `:0.1.6-amd64`, `:0.1.6-arm64`, `:0.1.6` | `gh run rerun <id> --failed`: it pushes the same saved images again |
| `attest-image` | the image, without its provenance | `gh run rerun <id> --failed` |
| `github-release` | at most a draft, which only you can see | `gh run rerun <id> --failed`: it finds its draft, uploads with `--clobber`, checks the list and publishes |
| `image-tags` | everything but the moving tags | `gh run rerun <id> --failed` |

`gh run rerun <id> --failed` re-runs the failed jobs and the ones after them, with the same run's artifacts (kept 14
days). A re-run of `sign-helper-manifest` waits for its reviewer again.

**Delete the tag and tag again** (only before `attest-files` has passed): `gh release view v0.1.6` must say "release
not found" (nothing was published), then

```bash
git push origin :refs/tags/v0.1.6
git tag -d v0.1.6
# fix, merge, then step 5 again on the new merge commit
```

## 7. Verify

```bash
python scripts/release_checksums.py 0.1.6 --verify
```

downloads `SHA256SUMS`, every file and its `.sha256` from the release and holds each to the sums, and checks that
PyPI holds exactly the release's five wheels and sdist with the same hashes. Then:

```bash
mkdir -p verify-0.1.6 && cd verify-0.1.6
gh release download v0.1.6 -p 'citar-0.1.6-cp311-abi3-manylinux_2_28_x86_64.whl' -p citar-helper-linux-x64
gh attestation verify citar-0.1.6-cp311-abi3-manylinux_2_28_x86_64.whl --repo jprodgers/CITAR
gh attestation verify citar-helper-linux-x64 --repo jprodgers/CITAR
gh attestation verify oci://ghcr.io/jprodgers/citar:0.1.6 --repo jprodgers/CITAR
```

- **pip takes a wheel, not the sdist:** in a fresh venv, `python -m venv /tmp/v && /tmp/v/bin/pip download --no-deps
  citar==0.1.6 -d /tmp/v/dl` downloads `citar-0.1.6-cp311-abi3-<platform>.whl`.
- **The image answers its health route** (in WSL, with Podman; this laptop has no Docker):

  ```bash
  podman run --rm -d --name citar-verify -p 127.0.0.1:18765:8765 ghcr.io/jprodgers/citar:0.1.6
  curl -fsS http://127.0.0.1:18765/api/auth/config && echo
  podman stop citar-verify
  ```

- **A helper from `releases/latest/download/` reports 0.1.6:**

  ```bash
  curl -fsSLO https://github.com/jprodgers/CITAR/releases/latest/download/citar-helper-linux-x64
  chmod +x citar-helper-linux-x64
  ./citar-helper-linux-x64 --version --home "$(mktemp -d)"     # citar-helper 0.1.6 (..., v0.1.6)
  ```

## 8. Package managers

Their manifests carry the hashes of files that exist only now. Either take them from the release run's preview, or
write them again from the published release:

```bash
gh run download <id> -n pkg-release-preview -D preview-0.1.6     # managers/ in it
# or: python scripts/release_checksums.py 0.1.6 --write          # beside the templates in packaging/
scripts/release/managers.sh 0.1.6 --manifests preview-0.1.6/managers   # or without --manifests, for packaging/
scripts/release/managers.sh 0.1.6 --manifests preview-0.1.6/managers --push
```

`managers.sh` clones the tap (`jprodgers/homebrew-citar`) and the bucket (`jprodgers/scoop-citar`) into a
temporary folder, copies the manifests in (the tap: `Formula/citar-helper.rb` added and 0.1.5's `Formula/citar.rb`
removed; the bucket: `bucket/citar.json` and `bucket/citar-helper.json`), shows the diff, and commits and pushes
only with `--push`. Read the diff before the second command.

winget, from a Windows machine with winget 1.12 or newer (an older one warns on every schema header) and
`wingetcreate` signed in (README.md, "winget"):

```powershell
winget validate --manifest packaging\winget\game
wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR 0.1.6" packaging\winget\game
winget validate --manifest packaging\winget\helper
wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR.Helper 0.1.6" packaging\winget\helper
```

(the manifests from `release_checksums.py 0.1.6 --write`, or copied there from the preview's `managers/winget/`).

## 9. Deploy

With the owner's go-ahead, from the release's wheel (design/phase5-release.md P5.13 and P5.14; the server's runbook,
[docs/server/RUNBOOK.md](../docs/server/RUNBOOK.md), has each command's options):

1. `python scripts/deploy/bundle.py 0.1.6`: the deploy bundle, the wheel fetched from the release and checked with
   `gh attestation verify` before anything reaches the server.
2. The staging rehearsal in WSL with that bundle (the runbook's "The staging rehearsal"), from a clone of the tag in
   WSL's own disk (`~/citar` below, holding the tag `v0.1.5` too; nothing under `/mnt`):
   - Unpack the bundle there (reading the tarball from `/mnt/c` is fine; nothing is written under `/mnt`):
     `mkdir -p ~/citar-staging && tar -xzf <the bundle>.tar.gz -C ~/citar-staging`. The folder
     `~/citar-staging/citar-deploy-0.1.6` is `run`'s `--bundle`, and it holds the release's wheel and constraints.
   - As your own account, the three planted releases (0.1.7-rc.1 to rc.3; about 20 minutes):
     `bash scripts/deploy/staging_wheels.sh --clone ~/citar --out ~/citar-staging --cutover-from ~/citar-staging/citar-deploy-0.1.6`.
   - As root, from copies of `scripts/deploy/staging.sh` and `staging_check.py` on WSL's disk (about 20 minutes):
     `staging.sh setup --ballast-mb <the other sites' memory, from the private deploy log> --repo ~/citar
     --cpu-quota <the last rehearsal's>`; `staging.sh probes --without-cpu-quota`, which must exit 0 (the CPU probe fails without the
     quota, and the quota is back); `staging.sh run --bundle ~/citar-staging/citar-deploy-0.1.6 --rc1
     ~/citar-staging/bundles/citar-deploy-0.1.7-rc.1 --rc2 ~/citar-staging/bundles/citar-deploy-0.1.7-rc.2 --rc3
     ~/citar-staging/bundles/citar-deploy-0.1.7-rc.3 --host-mb 128`, which must exit 0; then
     `staging.sh cleanup --keep-logs <a folder of your own>`.
3. Copy the bundle to the server; make `/etc/citar/deploy-checks.conf` from the private deploy log.
4. `sudo bash deploy/citar-deploy.sh cutover --bundle <dir> --plan`, read it, then the same without `--plan`.
   Rollback: `sudo citar-deploy uncutover`.

## 10. After

- The first administrator: `sudo citar-admin create-user ...`.
- The host's machine settings from the staging measurement: its memory figure **128 MB** (the Servers page, or
  `PUT /api/machines/<host>/settings` with `capabilities.cpu.memory_mb`), not server mode's default of 240, which
  with the server's own peak would pass `citar.service`'s `MemoryHigh=320M`.
- One lab game's seconds per round on the server compared with staging's, and the ratio recorded as the next
  rehearsal's `--cpu-quota`.
- Helpers re-registered with the new helper.
- The private deploy log written.

## Rehearsing the tag-only jobs

Nothing but a tag runs the publishing jobs, so they are held another way (design/phase5-release.md P5.18.4):

- `scripts/ci/check_workflows.py` (every pull request's lint) holds release.yml to its rules: no publishing job
  outside the tag guard, the environments by name, the needs graph and each job's permissions (rule 16), nothing
  installed or built where a secret or a write permission is held, every action pinned to a commit.
- `scripts/ci/release_sign.py` is tested with throwaway keys and the real xtask binary (`tests/test_release_sign.py`
  with `CITAR_TEST_XTASK` naming one), and `package.yml` proves the signing path with a throwaway key on every pull
  request.
- The dry run's `dry-run-summary` runs the tag-only jobs' file steps (`assemble_release.py sums` and `final`, and
  `check_image.py load`) on the run's real files.
