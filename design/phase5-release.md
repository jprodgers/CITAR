# CITAR 0.1.6, Phase 5: packaging, CI, deploy, docs (design)

**Status.** Design, 2026-10-18, for Phase 5 of `ops/plan-0.1.6.md` §7, written against `v0.1.6` at `51ce329` (Phases 0-4 complete; Phase 4's exit met in full, its deferred real-model gate passed at `8061578`). Sections are numbered P5.x, so they cannot be confused with DESIGN.md's Phase 1 (§1-§12), Phase 2 (P2.x), `design/phase3-features.md` (P3.x) or `design/phase4-helper.md` (P4.x). Appendix F is the work breakdown: thirteen packages, run strictly one after another, one agent at a time on this laptop (the owner's standing choice). Each package appends an "As built in 5-xx" note to the section it implements, records its wall-clock start and end, and ends with its gates, a review and a fix round.

This file lives in `design/`, outside `docs/`, so mkdocs and the wiki generator never see it. It names code paths in backticks rather than linking them, so `scripts/check_links.py` needs no change. It names no private detail of the VPS (address, accounts, key paths, the other sites' names): those stay in the gitignored `ops/DEPLOY_LOG.md`, and every script here takes them as arguments or from a file on the box.

**Sources:**
- `ops/plan-0.1.6.md` §1 (R4: the tap ships only the helper; the game installs from prebuilt wheels through `install.sh` or pipx), §7 Phase 5 (builds, VPS, docs, exit) and Phase 6 (what this phase must make mechanical), §9 risk 5 (the VPS runs wheels only).
- `crates/citar-engine/DESIGN.md` P2.6.7 and P2.6.8 (maturin, the CI's builds), "As built in 2-06b" (check_dist, the manylinux_2_28 wheel, the sdist's contents, "Left for later"), P2.9 "Packaging: now and Phase 5", P2.2.1 (the content build id; `CITAR_BUILD_ID` is a display label).
- `design/phase4-helper.md` P4.3.8 (the manifest, the `packaged` feature, `keygen`/`helper-manifest`/`sign-manifest`), P4.3.9 (services), P4.7 (the cross-cutting rules this design keeps), P4.10 "What Phase 5 builds", and the as-built notes of 4-13 and 4-14 (the CI signing path, the five release names, helper.yml).
- `design/phase3-features.md`'s Phase 5-6 notes (the release docs pass), `KNOWN_ISSUES.md`, `CHANGELOG.md`'s Unreleased section, `docs/server/WORKERS.md` "Signing helper updates".
- `ops/phase1-common.md`, `ops/p3-notes.md` and the last sections of `ops/p4-notes.md` (the build environment and the CI lessons: CI slack x3, `perf_counter`, `TZ=UTC0` and `C:/dev/venv/py312-check`, `fsutil.newest_first`, never sleep-then-assert, elevated Windows runners, macOS soft limits, `cargo doc -D warnings` on Linux, `cargo build` of a bin with required features needing `--bin` and `--features`), and `ops/DEPLOY_LOG.md` (the VPS as deployed, read for facts only).
- The current tree, read file by file (P5.0 cites it).

---

## P5.0 What exists today (checked against the tree at `51ce329`)

### P5.0.1 Versions and the build label

- **Two version sources held equal:** `[workspace.package] version = "0.1.5"` in `Cargo.toml` (every crate takes `version.workspace = true`) and `__version__ = "0.1.5"` in `citar/__init__.py`; `cargo xtask check` (`xtask/src/check/version.rs`) fails when they differ. `pyproject.toml` has `dynamic = ["version"]`, so the wheel's version is Cargo's.
- **Other places that carry the version, held by nothing:** the eleven workspace members' entries in `Cargo.lock`, and the three path packages (`citar-bot`, `citar-engine`, `citar-testkit`) in the fuzz crate's own `crates/citar-engine/fuzz/Cargo.lock`, which rust.yml checks `--locked`; `installer/citar.iss` (`#define AppVersion "0.1.5"` at `:15`, the comment `ISCC.exe /DAppVersion=0.1.5` at `:4`, and `VersionInfoVersion={#AppVersion}` at `:34`, which Inno Setup requires to be numeric); `docs/INSTALL.md:33` (`Get-FileHash .\CITAR-0.1.5-setup.exe`); the `KNOWN_ISSUES.md` header ("in 0.1.6"); the CHANGELOG's section and compare links. The 0.1.5 procedure bumped them by hand.
- **The engine's content code hashes the package version** (DESIGN.md P2.2.1), so a version bump moves the build id (`0b412a18fe95` today) and the jobs build. Nothing pins them where a bump would break it: the `jobs` golden set holds a result's build-naming keys to the running build's values (`crates/citar-jobs/tests/golden.rs:9-11`, "an edit that moves the build id and changes no game needs no bless"); `citar/jobs/costs.json`'s `build` is informational (`citar/jobs/store.py` never reads it); no Python test pins `__version__`.
- **The label:** `BUILD_LABEL` is `CITAR_BUILD_ID` at compile time, else `unknown` (`crates/citar-engine/src/rules/source.rs:71-74`). `cargo xtask develop` sets it from `git describe` (`xtask/src/develop.rs:386`); no CI build sets it, so every CI wheel and helper says `unknown` (2-06b's "Left for later": "the tag's label is Phase 5's"). It is shown by `citar doctor` (`citar/doctor.py:241`: "build X (label), version V"), carried in `build_info()`, and printed by `citar-helper --version` and its hello (`crates/citar-helper/src/version.rs:79-121`).

### P5.0.2 The package and its builds in CI

- **`pyproject.toml`** builds with maturin (`maturin>=1.15,<2`): the mixed layout, `module-name = "citar._engine"`, `features = []`, `editable-profile = "ci"`, the `include` lists, `license-files = ["LICENSE", "NOTICE.md"]`, `requires-python = ">=3.11"`, classifiers through 3.14. The extension is `abi3-py311` (`Cargo.toml`'s `pyo3` line): one wheel per platform serves 3.11 and every later CPython (not the free-threaded builds, which abi3 does not cover).
- **`test.yml`** (`MATURIN_VERSION: v1.15.0`):
  - `build-ext` (`:41-125`) builds the ci-profile test wheel with `test-ops` on ubuntu, windows and macos, and the dev-guard helpers for the end-to-end tests;
  - `test` (`:127-207`) runs the suite on Linux 3.11, 3.12 and 3.13 and on Windows and macOS 3.12; `interpreter-exit` covers 3.11-3.14; `lint` is ruff; `frontend` is Node;
  - `package` (`:290-395`) builds the sdist and **one** release wheel, manylinux_2_28 x86_64, and checks both with `scripts/ci/check_dist.py`;
  - `installer` (`:397-440`) runs `install.sh --from` with the **build-ext test wheel** (ci profile) on ubuntu and macOS;
  - `docker` (`:442-482`) builds the image from the manylinux wheel for amd64 only and checks its health route.
- **Not built anywhere:** the Windows, macOS and Linux aarch64 release wheels. No wheel is smoke-tested on the Python floor (3.11) or the newest (3.14), and no suite runs on 3.14, which the classifiers claim.

### P5.0.3 release.yml

The workflow (`.github/workflows/release.yml`, 398 lines) is 0.1.5's with Phase 4's helper jobs, behind a gate:
- **`verify`'s first step fails every run** while `__version__` is 0.1.6 or earlier ("release packaging is Phase 5", `:36-57`), and every job needs `verify`. Its suite runs on `pip install -e ".[all]"`, an editable ci-profile build **without** the test operations, so the engine's tests skip there.
- **`build-python`** (`:88-101`) runs `python -m build` on Linux: one `linux_x86_64` wheel (no manylinux tag; PyPI refuses it) and the sdist.
- **`build-windows`** (`:116-180`) bundles an **editable ci-profile** extension built into the checkout (`pip install -e`), so a bundle would ship the ci profile; Inno Setup's path is hard-coded to `Inno Setup 6`.
- **`build-helper`** (`:193-262`) is P4.10's five-target cargo matrix (4-14). No `CITAR_BUILD_ID`, so the label is `unknown`.
- **`sign-helper-manifest`** (`:272-314`) names `environment: helper-signing` **with no tag guard**: once the gate goes, a dry run would create that environment (it does not exist; a job naming a missing environment creates it) and, once protected, wait for the owner's approval.
- **`publish-image`** (`:317-354`) builds from the source under QEMU for arm64; the Dockerfile has since required `wheelhouse/` (2-06b), so it would fail.
- **`github-release`** (`:357-398`) collects `*.whl`, `*.tar.gz`, `*.exe`, `*.zip`, `*.sha256` and `citar-helper-*`; the notes are the CHANGELOG section (awk) plus checksums. `publish-pypi`, `publish-image` and `github-release` are guarded by `startsWith(github.ref, 'refs/tags/')` only: a `workflow_dispatch` **run on a tag ref** would pass that guard and publish.
- No attestations, no `SHA256SUMS`, no check that the build ids of the artifacts agree.

### P5.0.4 The Windows bundle and installer

- **`installer/citar.spec`** puts the checkout first on `sys.path` (`:28`) and in both `pathex` (`:84`, `:96`), so PyInstaller collects `citar` **from the checkout**, which holds `_engine.pyd` only after an editable build. Nothing names `citar._engine` or checks it was collected.
- **`installer/citar.iss`**: per-user (`PrivilegesRequired=lowest`), the PyInstaller folder whole, an unchecked "add to PATH" task (HKCU `Environment`), the uninstaller asking before deleting `%LOCALAPPDATA%\CITAR` (default No).
- **The bundle's lab cannot run** (P5.0.11).

### P5.0.5 The image

`Dockerfile` installs the dependencies from `pyproject.toml` (`scripts/ci/requirements.py server`), then `citar` from `wheelhouse/` with `--no-index` (`:50-54`), on `python:3.12-slim-bookworm` (Debian 12, past its regular security support since mid-2026); runs as uid 10001, health on `/api/auth/config`. `docker-compose.yml` pulls `ghcr.io/jprodgers/citar:latest` behind Caddy (`deploy/Caddyfile`). Only amd64 is ever built from a wheel.

### P5.0.6 The install scripts

- **`install.sh`** makes a venv under `~/.local/share/citar-app` (or `/opt/citar` with `--server`), installs `citar[all]` (or `[server]`) **from PyPI with no `--only-binary`** (`:176-179`), so on a platform without a wheel pip downloads the sdist and fails slowly at the Rust build; links the commands into `~/.local/bin` and appends a PATH line to the shell profile (`:200-232`); `--from` takes a local wheel or checkout (CI's route); `--worker` (`:59-65`) only prints where to download the helper and exits 2.
- **`install.ps1`** installs into `%LOCALAPPDATA%\CITAR\app` and writes the **user's PATH** (`:38-39`, `:149`): it has no `-Prefix`, no `-From` and no way to leave the PATH alone, so it cannot be tested on this laptop at all without touching the owner's real profile; `-Worker` prints the pointer.

### P5.0.7 Package managers and `release_checksums.py`

- `packaging/homebrew/citar.rb` installs **the whole game from the PyPI sdist** into a virtualenv (`:14`): since 0.1.6 that sdist needs a Rust toolchain the formula does not depend on, and R4 says the tap ships only the helper.
- `packaging/scoop/citar.json` (the portable zip) and `packaging/winget/` (three files, the Inno installer, `JimmieRodgers.CITAR`) carry the game only, at 0.1.5. Microsoft never merged a New-package PR for `JimmieRodgers.CITAR` (0.1.0 to 0.1.5 open).
- `scripts/release_checksums.py` hashes three assets (the PyPI sdist, the setup.exe, the zip: `ASSETS`, `:27-41`) and rewrites the three manifests in place; the sdist URL can 404 for a while after a release.

### P5.0.8 The helper's release side (P4.10's hooks)

- The five unversioned names (`citar-helper-windows-x64.exe`, `-macos-arm64`, `-macos-x64`, `-linux-x64`, `-linux-arm64`) are held equal by `citar/web/tests/helper.test.mjs` across `helperrules.js`, release.yml's `build-helper` matrix, helper.yml's `artifact` matrix and `cargo xtask helper-manifest`'s table.
- **The `packaged` feature** exists (`crates/citar-helper/Cargo.toml`; `config.rs:536`, `update/mod.rs:567`): `updates` defaults to `notify` and `auto` is refused. **Nothing builds it.**
- **xtask:** `keygen --out DIR` (refuses a folder in any git working tree), `helper-manifest --assets DIR --version V` (reads the build, protocol, version and target from the Linux asset, which must run where it is called), `sign-manifest (--key FILE | --key-env NAME)`. There is no verify command; the extension's `verify_manifest` trusts only the compiled-in keys (and, with test-ops, `CITAR_HELPER_UPDATE_KEY`).
- **`crates/citar-jobs/update-keys.pub` is empty**, so every helper reports updates `unavailable`. The owner's setup (environment `helper-signing` with a required reviewer and the secret `CITAR_UPDATE_SIGNING_KEY`, the public key committed) is `docs/server/WORKERS.md`'s "Signing helper updates"; the environment does not exist yet.
- `cargo xtask check` (`xtask/src/check/helper.rs`) refuses a test feature, `--all-features` or a feature list held in a variable on any line of release.yml and of helper.yml's artifact jobs; `check/triggers.rs` holds rust.yml's path filters to every file the checks read.

### P5.0.9 The deploy and the VPS

- **On the box** (facts from `ops/DEPLOY_LOG.md`; private details stay there): Ubuntu 24.04.5, 1 vCPU, 961 MB RAM, a 2 GB swapfile, x86_64, Python 3.12.3; nginx with certbot's TLS; three other sites that must never be disturbed. CITAR's code is a `git archive` of the tag extracted into `/opt/citar` with a venv at `/opt/citar/.venv`; state in `/var/lib/citar`; configuration and secrets in `/etc/citar/citar.env` (root:citar 0640); `citar.service` (user `citar`, `MemoryMax=400M`, a sandbox); a permanent `citar-lab.service` (0.1.5's runner, `MemoryMax=450M` of its own); `citar-backup` service and timer; a hand-made `/usr/local/bin/citar-admin` wrapper.
- **The tracked files** (`deploy/`): `citar.service` runs `python -m citar.server` with `WorkingDirectory=/opt/citar` (`:14-16`), so a wheel install would be shadowed by any `citar/` left in that tree (P2.9); `citar-lab.service` is already gone (4-09); `citar-backup.sh` snapshots the database, the env file and `saves/` only, so `config/`, `benchmarks/` and anything else 0.1.6 writes under the state directory is not backed up; `citar-secrets.sh`; `nginx-citar.conf`; `Caddyfile` (compose only).
- **The wizard's unit** (`citar/wizard/server.py:348-382`, `citar setup --server`) is a second, different definition (`Type=simple`, no `WorkingDirectory`, a shorter sandbox).
- **`requirements.txt`** exists only because "the service runs the code in place from /opt/citar" (its header); `scripts/sync_requirements.py` and test.yml's step keep it equal to `pyproject.toml`.
- **The procedure** (`docs/server/RUNBOOK.md:235-252`, the private log): `git archive`, `scp`, backup, extract, `pip install -r requirements.txt`, restart `citar` and `citar-lab`, check the other sites answer 200.
- **What 0.1.6 needs instead of `citar-lab`:** nothing. The lab runs inside the server: the dispatcher's local executor starts one job host child (`python -m citar.jobs host`, `citar/jobs/local.py`, `citar/jobs/host.py`) inside `citar.service`'s cgroup, under the host machine's CPU settings, server mode's default being one thread, 50% and min(1,024 MB, a quarter of the RAM), so about 240 MB on this box, held by `RLIMIT_DATA` (a system service has no user manager for a scope) and `nice` (`citar/machine_settings.py:159-186`). So `MemoryMax=400M` now holds the server **and** its games, where 0.1.5 gave CITAR 400M + 450M.

### P5.0.10 Docs and the wiki

`README.md`, `docs/INSTALL.md` and `packaging/README.md` describe 0.1.5's routes (Homebrew installing the game, `pip install` from an sdist that compiled nothing, no helper packages); `docs/server/DEPLOY.md` and `RUNBOOK.md` the `git archive` deploy; `CONTRIBUTING.md`'s "Releasing" (`:554-560`) the hand bump. `NOTICE.md` says "No dependency is vendored", while every wheel and helper now statically links about 150 Rust crates whose licences (MIT, Apache-2.0, BSD, ISC, Unicode) require their notices to travel with the binaries, and the Windows bundle carries the Python dependencies' code. `KNOWN_ISSUES.md` "Platforms and packaging" predates the wheels. The wiki is generated by `scripts/build_wiki.py` from 25 pages; mkdocs builds the site from `docs/`.

### P5.0.11 Defects found while reading (fixed in Phase 5)

1. **The Windows bundle's lab cannot start its job host.** `citar/jobs/host.py:86` starts `[sys.executable, "-m", "citar.jobs", "host"]`; in a PyInstaller bundle `sys.executable` is `citar.exe` or `citar-play.exe`, whose argument parser refuses `-m`. Every lab game of a bundled install fails at the host's start (5-05).
2. **`/api/meta` names a file no installed copy has and leaks paths.** It returns `mcp_script = <root>/citar_mcp.py` with `root = Path(__file__).parents[2]` (`citar/server/app.py:481-487`): in a wheel that is `site-packages/citar_mcp.py`, which does not exist (the file is the checkout's), so the lobby's MCP command (`citar/web/js/lobby.js:221-228`) is wrong for every pip, pipx, bundle and image install; and the route is public (`scripts/audit_routes.py:47`), so a public server tells anyone its install path and interpreter (5-05).
3. **A `workflow_dispatch` on a tag would publish** (P5.0.3); **a dry run would create `helper-signing`** (P5.0.3).
4. **Release builds carry the label `unknown`** (P5.0.1).
5. **No third-party notices ship with the binaries** (P5.0.10).
6. **The backup misses part of the state directory** (P5.0.9).
7. **`install.ps1` cannot avoid the user's PATH and profile** (P5.0.6), so nothing tests it outside CI.
8. **The Homebrew formula cannot build 0.1.6** (P5.0.7).
9. **Two unit definitions** (P5.0.9) that already differ.

### P5.0.12 This laptop, WSL and GitHub

- **Windows 11 x64:** Python 3.11, 3.12 and 3.14 (py launcher; PyInstaller in the global 3.14), Rust per `rust-toolchain.toml`, Node 24, gh, maturin 1.15. **No Docker, no Podman, no Inno Setup.** Nothing is installed on Windows; a package's venv (`C:/dev/venv/p-5-xx`) takes pip packages as every package's did.
- **WSL Ubuntu 24.04.5** (`wsl -d Ubuntu-24.04 -u claude`), Python 3.12.3 and systemd 255 as on the VPS, NAT networking, `claude` with `Linger=no`, 12 CPUs and 7.8 GB. **Missing, read at `51ce329`:** podman, docker, nginx, sqlite3, shellcheck, ruby, and `python3-venv` (as on a fresh VPS). Root (`wsl -u root`, inside WSL only) may `apt install` what a gate needs; each install is recorded in `ops/p5-notes.md`. Eight Phase 4 target folders sit in `~/target`; WSL's virtual disk grows and never shrinks by itself.
- **GitHub:** `jprodgers/CITAR` is public; environments `github-pages` (docs.yml's deploy from `main`) and `pypi` (PyPI's trusted publishing works through release.yml and `pypi`); **no `helper-signing`**; PR #5 (`v0.1.6` → `main`) runs test.yml on every push and rust.yml, helper.yml and determinism.yml on pushes their path filters match; releases v0.1.0 to v0.1.5 exist (0.1.5's with 14 assets); the image is `ghcr.io/jprodgers/citar`.

---

## P5.1 Goals, non-goals and exit

### P5.1.1 What Phase 5 delivers

1. **One release build** (`package.yml`, P5.3-P5.10) that makes every artifact a release ships, tests each on its own platform, and publishes nothing: five abi3 wheels built natively and the sdist, the Windows bundle and installer, ten helper builds (the five self-updating binaries and five `packaged` archives for package managers), the helper manifest with a dry-run signature, the image for amd64 and arm64, the install scripts run against those files, the package managers' manifests generated and installed from them, third-party notices, checksums and the release notes. It runs on every pull request, so no failure is left for the release to find.
2. **`release.yml`** (P5.11) reduced to a version check, the test and package workflows, and tag-only publishing jobs: the real signature, PyPI, the image, attestations, the GitHub release. A dry run (`workflow_dispatch`) builds everything and publishes nothing, creates no environment and waits for nobody.
3. **One version, one label** (P5.2): `scripts/release.py` checks and bumps every place the version lives; the label is `v<version>` on a tag.
4. **The deploy from the release wheel** (P5.13): a release layout on the server, units that cannot be shadowed by a source tree, scripted install, cutover from 0.1.5, rollback and checks, and a staging dry run of all of it in WSL under the VPS's limits (P5.14).
5. **The release checklist** Phase 6 follows (P5.15), every step a script or a command, each proven by a dry run.
6. **The docs pass** (P5.16), the wiki regenerated.

### P5.1.2 Owner decisions that bind this phase

- **R4:** the Homebrew tap ships only the prebuilt helper; the game installs from prebuilt wheels through `install.sh` or pipx.
- **Plan §7 Phase 5:** the maturin wheel matrix (win-x64, macOS arm64 + x64, manylinux x64 + aarch64 built natively); Docker from wheels; install scripts from wheels only; the Windows bundle carrying `_engine.pyd`; Scoop and winget; the helper matrix (the same names plus `macos-x64`); attestations; the VPS deploy installs the release wheel. **Exit:** a dry run of the release workflow green on every platform.
- **Risk 5:** the VPS runs wheels only; mitigation: CI-built wheels and a staging dry run in WSL.
- **The update-signing key** (owner's answer of 2026-10-14, 4-13): a GitHub Actions secret in the protected environment `helper-signing`, created by the owner, who commits the public key. Agents never make the real key, environments, secrets or repository settings.
- **Priorities:** stability and speed first; fix bugs rather than carry them; no other users yet, so packaging changes freely (no install compatibility with 0.1.5 is owed, but the CHANGELOG says what changes for someone coming from it); strictly one agent at a time; packages sized for one sitting with local, re-runnable gates; CI-only gates named.
- **Safety** (P5.18.1) and **dispatch** (P5.18.2) rules.

### P5.1.3 Non-goals (Phase 6's, or later)

- **Phase 6:** the soak and QA runs, the security review, the performance report against 0.1.5, **the VPS deploy itself** (with the owner's go-ahead), the version bump, the tag, PyPI, the image push, the GitHub release, the tap, the bucket and winget. Phase 5 makes each mechanical and proves it by a dry run; it performs none of them.
- **Not in 0.1.6:** code signing and notarisation (P5.6, KNOWN_ISSUES); musllinux, 32-bit, Windows ARM64 and free-threaded wheels (an unsupported platform is told so and pointed at the sdist); homebrew-core; a winget submission that Microsoft merges (not in our hands).

### P5.1.4 Exit criteria

| Criterion | Measured by |
|---|---|
| The release dry run is green on every platform | `gh workflow run release.yml --ref v0.1.6 -f version=0.1.6` (P5.18.2), watched to its end: `meta`, test.yml (Linux 3.11-3.14, Windows, macOS, interpreter-exit, lint, frontend) and every job of package.yml green, every tag-only job skipped, the environments unchanged. The run id and each job's conclusion in "As built in 5-13". |
| Every wheel and the sdist | Five wheels, each tagged as P5.3's table says, checked by `check_dist.py`, installed in a clean venv on its own runner under Python 3.11 and 3.14 and smoke-tested (P5.3.4); the sdist checked and built into a wheel that passes the same smoke; all five report one version, one label, one build id and one jobs build, and play the same seeded lab job to the same result hash. |
| The bundle and the installer | The bundle built from the Windows wheel carries `_engine.pyd`, runs `--version`, `where` and `doctor` (its job host verifiable); the installer compiled, installed silently per user into a temporary folder on the runner, run, and uninstalled cleanly; the portable zip. |
| The helpers and the manifest | Ten builds on five runners (each `--version` naming its target, version and label, `--self-test`); the manifest over the five; its throwaway-key signature verified by xtask and by an independent verifier, a one-byte change refused. |
| The image | Built natively for amd64 and arm64 from the wheels, each run: health within 60 s, engine without test operations, the right version and label, the job host verifiable. |
| The install scripts | `install.sh` on Linux x64 and arm64 and macOS arm64 and x64, `install.ps1` on Windows, against the built wheels and helpers served from loopback; the game runs, `--worker` installs a helper that runs, uninstall leaves nothing. |
| The package managers | The Scoop, winget and Homebrew manifests generated from the built files and validated; Scoop installs the game and the helper on Windows, Homebrew installs and tests the helper on macOS arm64, macOS x64 and Linux x64; winget's manifests validated (and installed where the runner has winget). |
| The staging deploy | In WSL, as root, under a slice of one CPU and 961 MB: a 0.1.5 layout cut over to the **CI-built** wheel with `citar-deploy`, health and the stand-in other sites green before and after every step, a game and a lab experiment played under the unit's sandbox with no OOM kill and the memory measured, an upgrade, a rollback, a backup restored, the cutover undone; everything cleaned up afterwards. |
| The bump is mechanical | `release.py bump 0.1.6` in a scratch clone; `cargo xtask check`, S7's nextest and goldens and S1 green there. |
| Nothing was published | No tag, release, PyPI upload, image push or push to another repository; `gh api .../environments` lists exactly `github-pages` and `pypi` before and after every dispatch (P5.18.2). |

---

## P5.2 Versions, the build label and `scripts/release.py`

**One source and a checked set.** Cargo's `[workspace.package] version` is the source: the wheel takes it (`dynamic = ["version"]`), every crate and so every helper takes it, and `cargo xtask check` already holds `citar/__init__.py` to it. Everything else that names the release is a checked set that one script rewrites:

| Place | Rule |
|---|---|
| `Cargo.toml` `[workspace.package] version` | the source |
| `Cargo.lock`, the eleven workspace members (no `source` line); `crates/citar-engine/fuzz/Cargo.lock`, its three path packages | equal (cargo `--locked` refuses otherwise) |
| `citar/__init__.py` `__version__` | equal (also `cargo xtask check`) |
| `installer/citar.iss` `#define AppVersion` | equal (the default for a hand build; CI passes `/DAppVersion` and `/DAppVersionNumeric`) |
| `KNOWN_ISSUES.md` header ("in X") | the source version or newer (it names the release being prepared) |
| `CHANGELOG.md` | `## [Unreleased]` exists; with a version given, `## [V] - YYYY-MM-DD` and the compare links `[V]` and `[Unreleased]` |

The comment in `citar.iss` and the example in `docs/INSTALL.md` become version-free (`X.Y.Z`, `<version>`), so they leave the set. The packaging manifests are not in it: they are written after the release from its files (P5.9).

**`scripts/release.py`** (standard library only, so every job can run it before anything is installed):
- `check [--version V]`: every place in the set agrees (with `--version`, equals V), each disagreement named with its file and line; exit 1 on any.
- `bump V --date YYYY-MM-DD`: refuses V not newer than the source; rewrites the set, the lock's members included; turns `## [Unreleased]` into `## [V] - date` with a new empty `## [Unreleased]` above it and the two compare links; then runs `check --version V`.
- `prepare --mode ci|dry-run|tag [--version V] [--date D]`: the first step of every job that builds or tests (P5.17). `ci` checks; `tag` checks against V and fails on any disagreement; `dry-run` bumps the runner's checkout to V when the source differs (printing a notice that the bump is in this runner only) and then checks. So a dry run of the branch at 0.1.5 builds exactly what a tag of the bumped branch would build, and proves the bump on every runner.
- `label --mode M --version V --sha S`: `v0.1.6` on a tag, `v0.1.6-dryrun+<sha7>` on a dry run, `ci+<sha7>` otherwise. Every release build sets `CITAR_BUILD_ID` to it (wheels, sdist test build, helpers, bundle, image), so `citar doctor`, `build_info()` and `citar-helper --version` name the release.
- `meta`: validates the requested version (`^\d+\.\d+\.\d+(-(alpha|beta|rc)\.\d+)?$`, refused otherwise before any job uses it), and prints the GitHub outputs every job reads: `version`, `pep440` (maturin's mapping: `-rc.1` is `rc1`), `numeric` (Inno's four-part `0.1.6.0`), `label`, `date` (one per run, so jobs that run either side of midnight agree), `mode`, `prerelease`.
- `notes V`: the CHANGELOG section of V, byte for byte (replaces release.yml's awk).
- `assets V [--json]`: the release's asset list (P5.10), the one list every job and check compares with.

**Tests** (`tests/test_release_script.py`): each place planted out of step fails `check` naming it; `bump` in a temporary copy moves every place and nothing else; `prepare`'s three modes; `notes` against the 0.1.5 section; `assets` against P5.10; the regex. test.yml's lint job runs `release.py check`, so a half-done bump fails every pull request.

---

## P5.3 `package.yml` and the wheel matrix

### P5.3.1 One workflow builds what a release ships

`.github/workflows/package.yml` (name `package`) holds every build a release publishes. Triggers: `pull_request`, `push` to `main`, `workflow_dispatch`, and `workflow_call` with the inputs `mode`, `version`, `date` and `label` (release.yml passes them; on its own it computes them in its `meta` job with `mode=ci`). **It publishes nothing**: workflow permissions `contents: read`, no environment, no secret, no registry login (`scripts/ci/check_workflows.py` holds it to that, P5.17). Its concurrency group is `package-${{ github.workflow }}-${{ github.ref }}`, so a run called by release.yml (where `github.workflow` is the caller's name) never shares a group with a push's run and cannot deadlock against its caller or be cancelled by a later push.

Every job starts with the checkout and `release.py prepare` (P5.2), then builds with `CITAR_BUILD_ID` set to the label. Artifacts keep 7 days on pull requests and 14 on dry runs.

### P5.3.2 The wheels

`PyO3/maturin-action` at `maturin-version: ${{ env.MATURIN_VERSION }}` (`v1.15.0`, test.yml's pin, moved deliberately), `--release --locked`, the rust-cache keyed per target, each **on its own architecture** (no QEMU, no cross-compilation):

| Platform | Runner | Build | Tag | Library |
|---|---|---|---|---|
| windows-x64 | `windows-latest` | `x86_64-pc-windows-msvc` | `cp311-abi3-win_amd64` | `citar/_engine.pyd` |
| macos-arm64 | `macos-15` | `aarch64-apple-darwin`, `MACOSX_DEPLOYMENT_TARGET=11.0` | `cp311-abi3-macosx_11_0_arm64` | `citar/_engine.abi3.so` |
| macos-x64 | `macos-15-intel` | `x86_64-apple-darwin`, `MACOSX_DEPLOYMENT_TARGET=11.0` | `cp311-abi3-macosx_11_0_x86_64` | `citar/_engine.abi3.so` |
| linux-x64 | `ubuntu-24.04` | `manylinux: 2_28` (the x86_64 container) | `cp311-abi3-manylinux_2_28_x86_64` | `citar/_engine.abi3.so` |
| linux-arm64 | `ubuntu-24.04-arm` | `manylinux: 2_28` (the aarch64 container, native; the job asserts `uname -m` is `aarch64` and passes the native image explicitly if the action would pick a cross image) | `cp311-abi3-manylinux_2_28_aarch64` | `citar/_engine.abi3.so` |

- **abi3 and its floor:** `abi3-py311`, so each wheel serves CPython 3.11 and later; `requires-python = ">=3.11"`.
- **manylinux_2_28** (glibc 2.28, 2018): the VPS (2.39), Debian 12 and 13, Ubuntu 20.04 and later, RHEL 8 and later. maturin's built-in audit refuses a wheel that needs more than its tag says.
- **macOS 11** for both architectures (the arm64 minimum; the x64 build raised to it so both say the same).
- **The label reaches the container** (`docker-options: -e CITAR_BUILD_ID`), and the smoke test (P5.3.4) asserts it arrived.
- Each wheel is checked by `scripts/ci/check_dist.py wheel` with its tag and library (2-06b's checker: exactly the tracked files under `citar/`, the library, the dist-info, PEP 639's license files), then uploaded as `wheel-<platform>`.

### P5.3.3 The sdist

`maturin sdist` on ubuntu after `prepare`, checked by `check_dist.py sdist`, uploaded as `sdist`. A second job, `sdist-build`, builds a wheel **from the sdist as a user would** (`pip wheel --no-deps citar-<V>.tar.gz` with the runner's rustup, which honours the sdist's `rust-toolchain.toml`; maturin from build isolation, floating within `<2` as a user's would) and runs the smoke test on it with `--label unknown` (a user's build carries no label).

### P5.3.4 Each wheel installed and smoke-tested on its own runner

On the runner that built it, for Python **3.11 and 3.14** (the floor and the newest), a fresh venv, `pip install --only-binary=:all: "<wheel>[all]"`, then `scripts/ci/smoke_wheel.py --version V --label L --out smoke-<platform>-<py>.json`, run from a temporary folder with `-P` (2-06b's lesson: never from the checkout), with `CITAR_DATA_DIR` and the other state variables in that folder:
- `citar` and `citar._engine` import from site-packages; `HAS_TEST_OPS` is false; `build_info()` gives V, L and no ruleset folder; the ruleset's counts are sane; the web client, the migrations and the collectors are in the package; the state directory is outside site-packages;
- `citar --version`, `citar where`, `citar doctor --quiet` exit 0;
- **a job host** started through `citar.jobs.host.JobHost` with tiny limits (one thread, 25%, 256 MB) reaches `ready` with `verifiable` true: its canary passed and it replayed the shortest golden job to the committed hashes on this platform (P4.4.3);
- **the same host plays one seeded lab job** (a duel of 30 rounds, the same spec on every platform) through the production path, and its result hash is recorded; then the host is closed;
- the JSON line records the version, label, build id, jobs build, result hash and platform, and is uploaded.

On linux-x64 under Python 3.12, a third venv installs `"<wheel>[server]"` and writes `pip freeze --exclude citar` to **`citar-<pep440>-server-constraints.txt`**, the exact dependency set the release was tested with: the image and the VPS deploy install with it (P5.7, P5.13).

### P5.3.5 What moves out of test.yml

test.yml's `package`, `installer` and `docker` jobs move into package.yml in 5-02, onto the release wheels (the installer jobs stop using the ci-profile test wheels), unchanged otherwise until 5-06 and 5-07 replace them. test.yml keeps the suite and its test-ops wheels (P5.17).

---

## P5.4 The helper's release builds and the signing path

### P5.4.1 Ten builds on five runners

package.yml's `helpers` matrix is P4.10's (the same five runners, targets and unversioned names; helper.yml's `artifact` matrix stays equal to it and `helper.test.mjs` reads both). On each runner, `cargo build --release --locked -p citar-helper --target T` with `CITAR_BUILD_ID` and, on macOS, `MACOSX_DEPLOYMENT_TARGET=11.0`, twice:
- **the self-updating build** (no features): copied to its unversioned name, with its `.sha256`; what the Servers page links (`releases/latest/download/<name>`), `install.sh --worker` installs, and the manifest lists;
- **the packaged build** (`--features packaged`, a literal feature list, which `cargo xtask check` allows: `packaged` is no test feature): packed into a **versioned archive**, `citar-helper-<V>-<platform>.tar.gz` (`.zip` on Windows), holding `citar-helper[.exe]`, `LICENSE`, `NOTICE.md` and `THIRD-PARTY-LICENSES.txt`, with its `.sha256`. Homebrew, Scoop and winget install these (P5.9); they never self-update (P4.3.8).

Both builds are run on their runner: `--version` must name the target, the version V and the label; `--self-test` passes; the packaged one's `update check` reports a packaged build with policy `notify`. The Fedora TLS check (4-14) stays on linux-x64. The two builds share every crate but `citar-helper` itself, so the second costs one crate and the link.

### P5.4.2 The manifest and a signature that proves the path without the key

package.yml's `helper-manifest` job (ubuntu, no environment, no secret) downloads the five self-updating builds and:
1. writes `citar-helper-manifest.json` with `cargo xtask helper-manifest --assets DIR --version V` (it runs the Linux build, so it runs on Linux);
2. makes a **throwaway key** with `cargo xtask keygen --out "$RUNNER_TEMP/throwaway"` (outside the checkout, which keygen insists on), signs with `cargo xtask sign-manifest --key ...`, and verifies the signature twice: with a new **`cargo xtask verify-manifest --pub FILE MANIFEST [SIG]`** (5-03; ed25519 over `CITAR-HELPER-MANIFEST-V1\0` and the bytes as published, the same domain citar-jobs' test holds equal) and with an independent verifier, `scripts/ci/verify_manifest.py` (the `cryptography` package's Ed25519), so the format helpers check is checked by code that shares nothing with the signer; a copy with one byte changed must fail both;
3. checks that a release build with the committed `update-keys.pub` behaves as the tree says: empty, `update check` reports updates unavailable; not empty, each line is a well-formed public key;
4. uploads `helper-manifest` (the manifest alone, which the release attaches) and `helper-manifest-dry-run` (the throwaway signature and public key, named so, never a release asset). The private half stays in `$RUNNER_TEMP` and dies with the runner; `check_workflows.py` refuses an upload path that could include it.

**The real signature is release.yml's alone** (P5.11): a tag-only job in the environment `helper-signing`, under its required reviewer, signs the manifest package.yml built, then verifies the signature against each line of the committed `update-keys.pub`:

| `update-keys.pub` | Secret | Outcome |
|---|---|---|
| empty | absent | a notice; no `.sig`; the release's helpers report updates unavailable (the tree's state today) |
| empty | present | **fail**: "commit the public key first" (the signature would be trusted by nobody) |
| a key | absent | **fail**: helpers would trust a key no release signs with |
| a key | present | sign; the signature must verify against a committed key, else **fail** ("the secret is not the committed key") |

A dry run skips this job, so the environment is never used or created and nobody is waited for; package.yml has proved everything up to the signature with the throwaway key, and `scripts/ci/release_sign.py` (5-09), which holds the four rules, is tested locally with throwaway keys.

---

## P5.5 Third-party notices

- **`cargo xtask licenses [--check]`** (5-04) writes `packaging/THIRD-PARTY-LICENSES.txt` from `cargo metadata --locked`: the normal-dependency closure of `citar-py` and `citar-helper` over every release target (Windows-only and Unix-only crates included), each crate's name, version and licence expression, and the text of its licence and NOTICE files from the registry's sources, in name order. A licence outside the allow-list (MIT, Apache-2.0 and its LLVM exception, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0, Unicode-DFS-2016, MPL-2.0, CC0-1.0, BSL-1.0, 0BSD, CDLA-Permissive-2.0) is refused by name. `--check` regenerates and compares; rust.yml's lint job runs it after `cargo fetch --locked`, so a lock change without the file fails.
- **Where it travels:** the wheels (`license-files`, so pip installs it in the dist-info and the image carries it), the bundle, every packaged helper archive, and the release as an asset. The Windows bundle also gets `PYTHON-LICENSES.txt` for the Python packages it bundles (`pip-licenses --with-license-file`, run in the bundle's build venv; P5.6).
- **`NOTICE.md`**'s "Third-party dependencies" says all this.

---

## P5.6 The Windows bundle and installer

**From the wheel, never the checkout.** package.yml's `windows` job (needs `wheels`):
1. a clean Python 3.12 venv (the bundle's Python, as 0.1.5's), `pip install --only-binary=:all: "<windows wheel>[all]"`, PyInstaller at a pinned version, pillow for `make_icon.py`, pip-licenses;
2. `installer/citar.spec` collects `citar` **from site-packages**: no checkout on `sys.path` or `pathex` (the entry scripts are still read from `installer/`); `citar._engine` named in `hiddenimports`, and the spec fails unless the analysis resolved it to the installed wheel's `_engine.pyd`; data from the installed package; `LICENSE`, `NOTICE.md`, `README.md`, `THIRD-PARTY-LICENSES.txt` and `PYTHON-LICENSES.txt` at the root; `--distpath` and `--workpath` under `$RUNNER_TEMP`;
3. **the bundle runs:** `citar.exe --version` (V), `where`, `doctor --quiet` with every state variable in a temporary folder; `_engine.pyd` present; `build_info()`'s label; and the job host started **from the bundle** (P5.0.11 item 1's fix);
4. **Inno Setup:** `ISCC.exe` from the runner image when present, else installed on the runner (`choco install innosetup`, a pinned version); `/DAppVersion=V /DAppVersionNumeric=N` (Inno's `VersionInfoVersion` must be numeric, so a prerelease such as `0.1.6-rc.1` no longer breaks the compile);
5. **a silent per-user install on the runner:** `CITAR-V-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER /DIR=<temp> /TASKS="" /LOG=<temp>`, then the installed `citar.exe --version` and `doctor --quiet`, the uninstall key `{9C1D2A54-...}_is1` present under HKCU, then `unins000.exe /VERYSILENT /SUPPRESSMSGBOXES` and the folder and key gone (the data prompt's default is No, so data stays);
6. **the portable zip** `CITAR-V-windows-x64.zip` (the folder `CITAR/`, for Scoop), each file with its `.sha256`, uploaded as `windows`.

**Product fixes that ship with it** (5-05):
- **The job host in a frozen app** (`citar/jobs/host.py`): when `sys.frozen`, the command is `citar.exe` beside the running executable with a hidden `jobs-host` command (added to `citar/cli.py`, calling `engine_api.run_job_host()`), started with `CREATE_NO_WINDOW`, so the windowed `citar-play.exe` starts its lab's host through the console `citar.exe` without a flashing window; otherwise `python -m citar.jobs host` as now.
- **`citar doctor` gains a "jobs" line**: it starts the job host for a moment and reports `verifiable` (the canary and the self-check) or why not. Every smoke test (wheels, sdist, bundle, image) then proves the job host on its platform through one command.
- **`/api/meta`** returns `version`, `label` and `mcp_command`, the command that starts this install's MCP bridge (a bundle: its `citar.exe mcp`; a wheel: the `citar-mcp` script beside its interpreter; a checkout: `python citar_mcp.py`), and in server mode no filesystem path at all (a remote player runs `citar-mcp` from their own install); `lobby.js` builds its commands from `mcp_command`.
- **`installer/citar.iss`:** the version only from `/DAppVersion` (its default held by `release.py check`), `VersionInfoVersion={#AppVersionNumeric}`.

Code signing stays out (KNOWN_ISSUES): SmartScreen warns, every asset has its SHA-256 and an attestation (P5.12).

---

## P5.7 The image

- **Base:** `python:3.12-slim-trixie` (Debian 13, glibc 2.41; bookworm's regular security support ended in mid-2026). The wheel's manylinux_2_28 tag runs there.
- **Dependencies:** `requirements.py server` as now, installed `--only-binary=:all:` and, when `wheelhouse/constraints.txt` is present (CI copies the release's constraints there, P5.3.4), with `--constraint`, so the image runs the dependency set the release was tested with. Then `citar` from `wheelhouse/` with `--no-deps --no-index` (pip takes the wheel that matches the build's architecture).
- **Built natively, no QEMU:** package.yml's `image` matrix builds `linux/amd64` on `ubuntu-24.04` and `linux/arm64` on `ubuntu-24.04-arm`, each from its own wheel, `docker buildx build --load`. Each image is run: the health route answers within 60 s; `citar._engine` has no test operations; `citar --version` and `build_info()` give V and L; `citar doctor --quiet` (the job host verifiable) as uid 10001; a restart keeps the volume's state. `docker compose config` checks `docker-compose.yml`.
- **What a tag pushes is what was tested:** on a dry run and a tag, each job saves its image (`docker save`, zstd) as the artifact `image-<arch>`; release.yml's tag-only jobs load those, push them by architecture and join them in one manifest list (P5.11). Pull requests skip the save.

---

## P5.8 The install scripts

**Wheels only.** `install.sh` and `install.ps1` install `citar[...]` with `--only-binary=citar` (the game's own sdist is never built on a user's machine; dependencies keep pip's defaults). Before pip runs, each checks the platform against the wheel matrix and says plainly when there is no wheel: Linux x86_64 or aarch64 with glibc 2.28 or newer (`getconf GNU_LIBC_VERSION`; musl is named), macOS 11 or newer on arm64 or x86_64, Windows with an x64 Python (an ARM64 Python is told to use an x64 one or the installer). Anything else gets one message: no prebuilt CITAR for this platform; `pip install citar` builds it from source with a Rust toolchain (rustup.rs). Python 3.11 or newer as before; pipx is documented as the alternative route (INSTALL.md), not used by the scripts, whose own venv does the same job with one dependency fewer.

**The helper (`install.sh --worker`, `install.ps1 -Worker`):** installs the helper alone, with no game: the platform's **self-updating** build (`citar-helper-<platform>`) from `releases/latest/download/` (or `releases/download/vX/` with `--version X`), its `.sha256` checked before anything is written, then `citar-helper[.exe]` in the bin folder, made executable, and the next step printed (`citar-helper setup`, then `citar-helper service install` for a service). Curl and `Invoke-WebRequest` leave no quarantine attribute, so macOS runs it without `xattr`. A package-managed helper is the packaged build (P5.9); this one updates itself.

**Testable without touching anything real:**
- `install.sh`: `--prefix`, `--bin-dir`, `--no-profile` (no shell profile line), `--from FILE|DIR` (a wheel, or a folder used as `--find-links`; dependencies still from PyPI), and `CITAR_RELEASE_URL` (the base for helper downloads; `https`, or `http` to a loopback host only).
- `install.ps1`: `-Prefix`, `-BinDir`, `-NoPath` (the user's PATH and the registry untouched), `-From`, the same `CITAR_RELEASE_URL`; `-Uninstall` honours `-Prefix` and `-BinDir`. A test wrapper on this laptop refuses to run the script without `-Prefix` under the package's target folder and `-NoPath`.

**`--server`** keeps installing a server into the release layout of P5.13 (5-10 moves it there).

---

## P5.9 Package managers

| Manager | Package | What it installs | Manifest |
|---|---|---|---|
| Scoop | `citar` | the game: the portable zip (as 0.1.5) | `packaging/scoop/citar.json` |
| Scoop | `citar-helper` (new) | the packaged helper: `citar-helper-V-windows-x64.zip` | `packaging/scoop/citar-helper.json` |
| winget | `JimmieRodgers.CITAR` | the game: the Inno installer, per user (as 0.1.5) | `packaging/winget/game/` (three files) |
| winget | `JimmieRodgers.CITAR.Helper` (new) | the packaged helper: the zip, `NestedInstallerType: portable`, command alias `citar-helper`, per user | `packaging/winget/helper/` (three files) |
| Homebrew | `citar-helper` (new; the tap's only formula) | the packaged helper for macOS arm64 and x64 and Linux x64 and arm64 (`on_macos`/`on_linux` × `on_arm`/`on_intel`, each its archive's URL and SHA-256); `bin.install "citar-helper"`; the notices into `doc`; a `service` block running the helper in service mode (`brew services start citar-helper` keeps it running through the `opt` path, which survives upgrades, where `citar-helper service install` would point at a Cellar path that the next upgrade removes; the caveats say so); `test do` runs `--version` and `--self-test` | `packaging/homebrew/citar-helper.rb` |

- **The old `citar` formula goes:** `packaging/homebrew/citar.rb` is deleted here, and Phase 6 removes `Formula/citar.rb` from the tap with the tap's README saying why. Nobody else uses it (the owner's own copy is uninstalled with `brew uninstall citar`); the CHANGELOG's Compatibility says how to move (P5.16).
- **Packaged builds only** in package managers: their updates come through the manager (`brew upgrade`, `scoop update`, `winget upgrade`), and the helper refuses `auto`.
- **`scripts/release_checksums.py`** is rewritten around templates (`packaging/**/*.in`, P5.10's names and a `{{version}}`/`{{sha256:<asset>}}`/`{{url:<asset>}}` vocabulary): `--from-dir DIR` hashes built files (CI's route, before anything is published), `--base-url URL` points the manifests elsewhere (a dry run's loopback server), `--out DIR` writes them (`--write` writes into `packaging/` for review), and `--verify V` downloads the published release and checks every asset against `SHA256SUMS` and PyPI's JSON API hashes (Phase 6, after publishing). No PyPI sdist URL remains in any manifest, so the 404 lag is gone.
- **Validated in CI** (package.yml's `managers` job, P5.10): Windows: Scoop installed on the runner, `scoop install` of both manifests from a loopback server (`citar --version`, `citar-helper --version`), `winget validate` of both sets and, when the runner has winget, `winget install --manifest`; the winget YAML validated against Microsoft's JSON schemas named in each file's header (fetched in the job) with `jsonschema`. macOS arm64, macOS x64 and Linux x64: `brew tap-new --no-git citar/dryrun`, the formula pointed at the loopback server, `brew install`, `brew test`, `brew audit --strict` (offline), `brew style`. Linux arm64's formula branch is checked by its structure test only.
- **Submission** stays Phase 6's (P5.15): copies into clones of `jprodgers/homebrew-citar` and `jprodgers/scoop-citar`, and `wingetcreate submit` for each winget package. Microsoft has not merged the earlier New-package pull requests; submitting costs minutes, so Phase 6 submits both and records the PR numbers.

---

## P5.10 Assembling a release

**The asset list** (`release.py assets V`; `P` is V's PEP 440 form; every file but the `.sha256` files and `SHA256SUMS` has a `.sha256` beside it):

| Group | Files |
|---|---|
| Python | `citar-P.tar.gz`; `citar-P-cp311-abi3-win_amd64.whl`; `citar-P-cp311-abi3-macosx_11_0_arm64.whl`; `citar-P-cp311-abi3-macosx_11_0_x86_64.whl`; `citar-P-cp311-abi3-manylinux_2_28_x86_64.whl`; `citar-P-cp311-abi3-manylinux_2_28_aarch64.whl`; `citar-P-server-constraints.txt` |
| Windows | `CITAR-V-setup.exe`; `CITAR-V-windows-x64.zip` |
| The helper, self-updating (unversioned, P4.10) | `citar-helper-windows-x64.exe`; `citar-helper-macos-arm64`; `citar-helper-macos-x64`; `citar-helper-linux-x64`; `citar-helper-linux-arm64`; `citar-helper-manifest.json` (and `citar-helper-manifest.json.sig` on a signed tag) |
| The helper, packaged | `citar-helper-V-windows-x64.zip`; `citar-helper-V-macos-arm64.tar.gz`; `citar-helper-V-macos-x64.tar.gz`; `citar-helper-V-linux-x64.tar.gz`; `citar-helper-V-linux-arm64.tar.gz` |
| Notices and sums | `THIRD-PARTY-LICENSES.txt`; `SHA256SUMS` |

**package.yml's `assemble` job** (needs every build) downloads the artifacts into `release/` under exactly these names and fails on a missing or an extra file, naming it (`scripts/ci/assemble_release.py`). It then checks that **every artifact is one build**: the five wheels' and the sdist build's smoke lines, the ten helpers' `--version` lines, the bundle's and both images' report one version V, one label L, one build id and one jobs build (so the VPS's job host and every released helper can share CPU work, P4.4.5), and the five wheels played the seeded lab job to one result hash. It writes `SHA256SUMS`, the **release notes** (`release.py notes V`, then the checksums, `gh attestation verify` instructions and the install routes), and the package managers' manifests from the files (`release_checksums.py --from-dir release --out managers`, the real release URLs). It uploads `release-assets` (every file), `python-dists` (the wheels and the sdist, for PyPI) and `release-preview` (notes, sums, manifests, the list with sizes). The `managers` job (P5.9) uses `release-assets` behind a loopback server.

---

## P5.11 release.yml end to end

**The Phase 5 gate goes.** release.yml keeps its triggers (`push` of `v*` tags; `workflow_dispatch` with `version`), workflow permissions `contents: read`, and gains `concurrency: release-${{ github.ref }}` with `cancel-in-progress: false` (a release is never cut off mid-publish). **It builds nothing itself**: every build is package.yml's, every test test.yml's, both called, so a release can contain nothing a pull request did not build and test.

**The tag guard.** Every job that publishes, names an environment, holds a secret or writes anywhere has the job-level condition `github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')` (a dispatch on a tag ref is a dry run too), never under `always()` or `!cancelled()`. `check_workflows.py` proves it before any dispatch (P5.17).

| Job | Needs | On a tag | On a dry run | Writes / holds |
|---|---|---|---|---|
| `meta` | | `release.py meta` with the tag; `prepare --mode tag` (the tag, the source and the changelog agree) | `meta` with the input; `prepare --mode dry-run` | nothing |
| `tests` (calls test.yml) | `meta` | yes | yes | nothing |
| `package` (calls package.yml) | `meta` | yes | yes | nothing |
| `sign-helper-manifest` | `package` | yes: environment `helper-signing` (its reviewer approves), P5.4.2's four rules | skipped | the secret |
| `publish-pypi` | `tests`, `package`, `sign-helper-manifest` | yes: environment `pypi`, `python-dists`, `twine check`, trusted publishing (`skip-existing`, so a re-run after a partial upload completes it; PyPI's own attestations on) | skipped | PyPI (OIDC) |
| `publish-image` | `tests`, `package`, `sign-helper-manifest` | yes: loads `image-amd64` and `image-arm64`, pushes `:V-amd64` and `:V-arm64`, joins them as `:V`, `:<major.minor>` and (not for a prerelease) `:latest` (`docker buildx imagetools create`) | skipped | `packages: write` |
| `attest` | `publish-image` | yes: `actions/attest-build-provenance` over every file of `release-assets` and the signature, and over the image's manifest-list digest (`push-to-registry`) | skipped | `id-token`, `attestations: write` |
| `github-release` | `publish-pypi`, `attest` | yes: a **draft** with every asset and the signature, the list compared with `release.py assets V`, the notes from `release-preview`; then published, and marked latest unless a prerelease | skipped | `contents: write` |
| `dry-run-summary` | `tests`, `package` | skipped | yes: the job summary: V, L, the asset list with sizes and hashes, the image tarballs loaded and their architectures, what each tag-only job would do and with which environment | nothing |

- **Order on a tag:** nothing becomes public until the signature passes its rules: a signing failure (the wrong key, a missing public key) stops the release with PyPI, the registry and the release page untouched. The GitHub release is published last and as a whole, so `releases/latest/download/<helper>` never points at a release that lacks a helper.
- **PyPI's trusted publisher** names release.yml and `pypi`, so publishing stays in release.yml (a reusable workflow cannot publish under it).
- **Prereleases** (`-rc.N` and the like) publish everything but are never `latest` on GitHub or in the registry.
- **The notes** are the CHANGELOG section (P5.2) with the checksums and verification lines (P5.10).

---

## P5.12 Attestations

- **Made only for a tagged release** (`attest`): build provenance for every release file and the image digest, by `actions/attest-build-provenance` with the run's OIDC identity. Attestations of a public repository go to Sigstore's public transparency log, which is publishing: **a dry run makes none.** It proves the inputs instead: `assemble` computes the subject list (every file of `release-assets`), and `dry-run-summary` shows it.
- **PyPI** adds its own attestations (PEP 740) through trusted publishing.
- **Verification** is documented (INSTALL.md, the release notes) as `gh attestation verify <file> --repo jprodgers/CITAR`, and Phase 6's deploy bundle runs it on the wheel before anything reaches the VPS (P5.13.5).

---

## P5.13 The deploy from the release wheel

### P5.13.1 The layout on the server

```
/opt/citar/releases/<pep440>/venv/            a venv per release, root-owned, read-only to the service
/opt/citar/releases/<pep440>/wheel, freeze.txt, SHA256SUMS line
/opt/citar/current -> releases/<pep440>       the release that runs (switched atomically)
/var/lib/citar/                               all state (citar:citar 0750); the units' working directory
/etc/citar/citar.env                          configuration and secrets (root:citar 0640), unchanged
/etc/citar/deploy-checks.conf                 the other sites' URLs, one a line (made on the box, never tracked)
/usr/local/sbin/citar-deploy                  the deploy script (installed by itself)
/usr/local/bin/citar-admin                    the admin wrapper
/var/log/citar-deploy.log                     one line a deploy: version, time, previous, backup
```

The units run console scripts from `/opt/citar/current/venv/bin` with `WorkingDirectory=/var/lib/citar`, so no source tree is ever on the import path (P2.9). `install.sh --server` and `citar setup --server` use the same layout.

### P5.13.2 The units and wrappers (`deploy/`)

- **`citar.service`:** `ExecStart=/opt/citar/current/venv/bin/citar serve` (settings read `CITAR_HOST` and `CITAR_PORT` from the env file, `citar/settings.py:265-266`), `WorkingDirectory=/var/lib/citar`, the sandbox as today, `MemoryMax`/`MemoryHigh` from the staging measurement (P5.14: kept at 400M/320M unless the server and one lab game measured together need more; the measured peaks and the rule are recorded beside them). The wizard renders this file's directives (a test holds the two equal), so there is one definition.
- **No `citar-lab` unit.** The lab's job host is a child of the server, inside its `MemoryMax`, under the host machine's CPU settings; Phase 6 sets those from the staging measurement after the fresh database exists (P5.15).
- **`citar-backup.sh`:** the database by `sqlite3 .backup` with its integrity check (as today), plus **the whole state directory** as a tarball without the live database files, the env file, and the release that wrote them (`readlink /opt/citar/current`); pruning after the new backup, as today.
- **`citar-admin.sh`** (tracked; installed as `/usr/local/bin/citar-admin`): `systemd-run --pty --wait --collect --quiet` as `citar` with the unit's `EnvironmentFile`, `WorkingDirectory` and file-system sandbox, so an admin command parses the env file exactly as the service does and can write nothing the service cannot.
- **`citar-secrets.sh`**, **`nginx-citar.conf`** unchanged in substance; the deploy never rewrites an installed nginx site (certbot edited it) and shows a difference instead.

### P5.13.3 `deploy/citar-deploy.sh`

One script, run as root on the box from an unpacked deploy bundle (P5.13.5) or from its installed copy. Every command has `--plan`, which prints each step it would take, with every path and command, and changes nothing (5-10 holds the plans to committed expected texts, as 4-14 held the service plans).
- **`install --bundle DIR`** (an ordinary release):
  1. **Preflight:** root; the bundle's files against its `SHA256SUMS`; the wheel's name, version and platform tag (a version older than the running one is refused: going back is `rollback`'s); `python3.12` with `venv`; `sqlite3`; at least 1.5 GB free under `/opt` and `/var`; the running release (if any); every URL of `deploy-checks.conf` answers 200 **before** anything changes (a box already in trouble is not deployed to), and CITAR's own health.
  2. **Backup:** `systemctl start citar-backup` (oneshot, so it waits); the new backup must exist without `UNTRUSTWORTHY`.
  3. **Stage:** `releases/<P>/venv` made fresh (an existing one is refused without `--reinstall`); `pip install --no-cache-dir --only-binary=:all: --constraint <constraints> "<wheel>[server]"` inside `systemd-run --scope -p MemoryMax=500M -p CPUWeight=20` so the install cannot starve the other sites; `pip freeze` kept.
  4. **Verify the staged release** as `citar` with the env file (read-only): `citar --version` is V; `citar._engine` loads from that venv without test operations; `citar doctor --quiet` passes, its job host verifiable (a FAIL line stops the deploy). In a cutover the fresh state has no database yet, so the doctor runs after step 6's first start instead, and a FAIL there is a failed health check.
  5. **Switch:** `current` replaced atomically (`ln -sfn` to a temporary name, `mv -T`); the units, `citar-admin` and the script itself installed from the bundle when they differ (the difference shown); `daemon-reload`; `systemctl restart citar`.
  6. **Health:** `/api/auth/config` 200 on 127.0.0.1 and through nginx (with the site's `Host`), and `/api/meta`'s version is V, within 60 s; a WebSocket handshake through nginx is answered by the app (an `Upgrade` the proxy passes). **On failure:** `current` goes back to the previous release, CITAR restarts, the health is checked again, and the script exits 1 naming what failed.
  7. **The other sites:** every URL again; a failure is reported loudly with the rollback command and exit 3, and nothing is done automatically (the cause may not be CITAR's; Phase 6 is attended).
  8. **Record** the deploy; prune releases beyond three, never `current` or the one before it.
- **`cutover --bundle DIR`** (Phase 6's first deploy, from 0.1.5's layout): the preflight, which also recognises the 0.1.5 layout (`/opt/citar/citar/__init__.py`, `/opt/citar/.venv`, `citar-lab.service`); the backup; stop `citar` and `citar-lab`; copy the installed unit files into the archive; disable and remove `citar-lab.service`; move `/opt/citar` to `/opt/citar-0.1.5`; move `/var/lib/citar` to `/var/lib/citar-archive-<stamp>` and make a fresh `/var/lib/citar` (the owner's "archive everything and start fresh"; the 0.1.6 server makes its database on first start); list the env file's keys that 0.1.6's `env.example` does not name (shown, not changed); then `install`'s steps 3-8 with `uncutover` as the rollback; print the next steps (P5.15).
- **`rollback [--to P] [--restore-backup DIR]`:** `current` to the previous (or named) release, restart, health; with a backup, the service stopped and the database and state restored from it first (a release after a migration needs its backup).
- **`uncutover`:** the cutover undone: the new layout and state moved aside, `/opt/citar-0.1.5` and the archived state put back, the saved units restored (`citar-lab` included), both started, health and the other sites checked.
- **`status`**, **`prune --keep N`**.

### P5.13.4 Limits on a 1 vCPU, 961 MB box

| Thing | How it is held |
|---|---|
| The server and its games | `citar.service`'s `MemoryMax`/`MemoryHigh` and `OOMScoreAdjust=500` (CITAR dies first, never the other sites' services); the job host's `RLIMIT_DATA` and `nice` inside it |
| The lab | the host machine's CPU settings (server mode: one thread, 50%, the memory figure Phase 6 sets from the staging measurement so that the server's peak plus the host's figure stays under `MemoryMax` with 60 MB to spare) |
| A deploy's pip install | a transient scope, `MemoryMax=500M`, `CPUWeight=20` |
| Releases on disk | three kept (a release's venv is about 200 MB) |

### P5.13.5 Where the wheel comes from

The laptop, never the box, fetches and checks it: `scripts/deploy/bundle.py V` downloads the release's linux-x64 wheel, `citar-P-server-constraints.txt` and `SHA256SUMS` (`gh release download vV`), checks the hashes, runs `gh attestation verify` on the wheel, adds `git archive vV deploy/ env.example`, and writes `citar-deploy-V.tar.gz` with its SHA-256 under `C:/dev/target`. `--from-dir DIR` builds a bundle from a dry run's artifacts or a local wheel (no attestation exists; it says so) for the staging run. Copying it to the box uses the operator's SSH details from the private log.

---

## P5.14 The staging dry run in WSL

**`scripts/deploy/staging.sh setup|run|cleanup`**, run as root inside WSL only (`wsl -d Ubuntu-24.04 -u root -- bash <copy in /root>`), on a copy of the branch and a deploy bundle, never under `/mnt/c`:
- **setup:** the apt packages a fresh VPS image lacks here (`python3-venv`, `sqlite3`, `nginx`), recorded; a **`citar-staging.slice`** with `CPUQuota=100%` (one CPU's worth; `AllowedCPUs=0` as well where the cpuset controller is delegated), `MemoryMax=961M` and `MemorySwapMax=2G`, which every staged unit joins through a drop-in (`Slice=`), so the deploy script runs unmodified; **two stand-in other sites** (an nginx server block and a small Python HTTP unit, each answering 200 on its own loopback port) listed in `/etc/citar/deploy-checks.conf`; **a 0.1.5 deployment** as the VPS has it: `git archive v0.1.5` into `/opt/citar`, its `.venv` with its `requirements.txt`, its `citar.service` and `citar-lab.service` (CITAR on `127.0.0.1` only), started once to make its state, and an nginx site for CITAR on a loopback port with the `connection_upgrade` map.
- **run:** `citar-deploy cutover` with the bundle (a local wheel in 5-11, **the dry run's CI-built wheel** in 5-13); then, through `scripts/deploy/staging_check.py` (standard library only): the first administrator made with `citar-admin`, a 4-bot small game played for 40 rounds through the API, a lab experiment of three duel games submitted as `citar` and played by the job host under the unit's sandbox, and the measurements (`MemoryPeak` and `MemoryCurrent` of `citar.service` idle, with the game, with the lab; the job host's peak from its frames; the slice's `memory.events` `oom_kill`, which must stay 0; seconds per round; the pip install's time and peak); `citar-deploy install` of a newer version (an `-rc` wheel built locally in the maturin container from a scratch clone bumped past the cut-over one) as an upgrade, `rollback`, `install` again; `citar-backup` and a restore drill (the snapshot opened, integrity-checked, and a second CITAR started against a copy on another port, reading its accounts); `uncutover`, with 0.1.5 answering again. The stand-in sites answer 200 before and after every step, and the CITAR site's WebSocket upgrade passes nginx.
- **cleanup:** units, drop-ins, the slice, the `citar` user, `/opt/citar*`, `/var/lib/citar*`, `/etc/citar`, the nginx sites and the stand-ins removed; nginx stopped and disabled (the packages stay, recorded); S10's WSL items checked. The logs and measurements are copied to `C:/dev/target/p-5-xx-staging/`.

Phase 6 runs it again with the published wheel before touching the VPS (P5.15).

---

## P5.15 The release checklist Phase 6 follows

`packaging/RELEASING.md` (5-09; walked through in dry-run form by 5-13) holds this list, each step a command:

1. **Before:** Phase 6's soak, QA and security review done; the owner's go-ahead. The version number decided (owner question). If updates are to work in this release: the owner has made `helper-signing` (required reviewer, the secret) and merged the public key into `update-keys.pub` (WORKERS.md); `gh api repos/jprodgers/CITAR/environments` shows it.
2. **Bump** on a branch from `main` (after `v0.1.6` merged): `python scripts/release.py bump 0.1.6 --date <today>`; review the diff; `release.py check --version 0.1.6`; `scripts/check_links.py --strict`; a pull request; every workflow green on it.
3. **Dry run** of that branch: `gh workflow run release.yml --ref <branch> -f version=0.1.6`, watched to green.
4. **Merge** to `main`; `main`'s CI green.
5. **Tag** the merge commit: `git tag -a v0.1.6 -m "CITAR 0.1.6"`, `git push origin v0.1.6`.
6. **Watch** release.yml; approve `sign-helper-manifest` when it waits for its reviewer.
7. **Verify:** `python scripts/release_checksums.py 0.1.6 --verify` (every asset against `SHA256SUMS`, PyPI's hashes against the built files); `gh attestation verify` on a wheel and a helper; `pip install citar==0.1.6` in a fresh venv takes a wheel, not the sdist; `docker run ghcr.io/jprodgers/citar:0.1.6` answers its health route; a helper from `releases/latest/download/` reports 0.1.6.
8. **Package managers:** the `release-preview` artifact's manifests (or `release_checksums.py 0.1.6 --write`); `scripts/release/managers.sh 0.1.6` clones the tap and the bucket into a temporary folder, copies the files (the tap: `Formula/citar-helper.rb` added, `Formula/citar.rb` removed; the bucket: `citar.json`, `citar-helper.json`), shows the diff, and pushes only with `--push`; `wingetcreate submit` for `JimmieRodgers.CITAR` and `JimmieRodgers.CITAR.Helper` (`winget validate` first).
9. **Deploy** (with the owner's go-ahead): `scripts/deploy/bundle.py 0.1.6`; the staging rehearsal with that bundle (P5.14); copy to the box; `/etc/citar/deploy-checks.conf` made from the private log; `sudo bash deploy/citar-deploy.sh cutover --bundle <dir> --plan`, read, then without `--plan`. Rollback: `sudo citar-deploy uncutover`.
10. **After:** the first administrator (`sudo citar-admin create-user ...`); the host's machine settings from the staging measurement; helpers re-registered with the new helper; `ops/DEPLOY_LOG.md` written.

---

## P5.16 The docs pass

Each file is read whole against the code at 5-12's base; every statement about installing, building, releasing, deploying or the 0.1.5 engine and worker is checked; what changes:

| File | What changes |
|---|---|
| `README.md` | Install: wheels for five platforms, the installer, `install.sh`, pipx, Docker (multi-arch), the helper's packages; "building from source needs Rust"; release verification in one line |
| `DESIGN.md` | Read for stale statements; a line on packaging pointing at ARCHITECTURE |
| `CONTRIBUTING.md` | "Releasing" points at `packaging/RELEASING.md` and `release.py`; package.yml beside test.yml; `check_workflows.py`; the smoke test; building wheels locally (maturin, the manylinux container in WSL); `requirements.txt` gone |
| `SECURITY.md` | The release chain: attestations, `SHA256SUMS`, the update key's custody and its rules (P5.4.2), the packaged helper; `/api/meta` no longer reveals paths; supported versions |
| `NOTICE.md` | Third-party notices (P5.5) |
| `CHANGELOG.md` | **Compatibility** gains "Installing and packaging" (below); Added and Fixed for Phase 5 (the wheels, the image, the helper packages, the deploy, P5.0.11's defects) |
| `KNOWN_ISSUES.md` | "Platforms and packaging" rewritten: wheel platforms and floors, no musl/32-bit/Windows-ARM64/free-threaded wheels, unsigned and unnotarised binaries, winget pending at Microsoft, package managers lag the release by the manifest step; "Server": one game a core, the lab inside `MemoryMax`, local backups |
| `docs/INSTALL.md` | Every route for 0.1.6: the installer, `install.sh`/`install.ps1` (wheels only, the platform table, `--worker`), pipx, Homebrew (helper only), Scoop and winget (game and helper), Docker, from source; verifying downloads; removing each |
| `docs/QUICKSTART.md`, `FAQ.md`, `TROUBLESHOOTING.md` | Install lines; "no wheel for my platform"; SmartScreen and quarantine; a helper from a package manager not updating itself |
| `docs/PLAYING.md`, `AI_PLAYERS.md` | The MCP command from `/api/meta` (`citar-mcp`, the bundle's `citar.exe mcp`) |
| `docs/ARCHITECTURE.md` | A "Packaging and releases" section: the build matrix, package.yml and release.yml, the label, the server's release layout |
| `docs/API.md` | `/api/meta`'s new fields |
| `docs/CONFIGURATION.md` | `CITAR_BUILD_ID` (build time), `CITAR_RELEASE_URL` (install scripts) |
| `docs/BENCHMARKS.md`, `BOTS.md`, `research/BOT_TUNING.md`, `REPORTS.md`, `SCENARIOS.md`, `MODDING.md`, `reference.md` | Read; install and lab-runner statements corrected (the lab inside the server; modding from a wheel install with `CITAR_RULESET_DIR`); `reference.md`'s modules exist |
| `docs/server/DEPLOY.md` | The three routes on the wheel; the release layout; `citar-deploy`; the units; limits on a small box |
| `docs/server/RUNBOOK.md` | "Deploying a new version" and rollback with `citar-deploy`; restores from the new backup; the staging rehearsal; the five commands' paths |
| `docs/server/VPS.md` | Capacity with the lab inside the server; swap; disk for three releases |
| `docs/server/WORKERS.md` | The helper's packages and `--worker`; "Signing helper updates" with P5.4.2's rules and the dry run |
| `docs/server/ACCOUNTS.md`, `OAUTH.md` | Read; `python -m citar.server` replaced by `citar serve` where a command is shown |
| `packaging/README.md` | Rewritten for 0.1.6: every route, what CI builds, what Phase 6 copies where |
| `wiki/` | Regenerated by `scripts/build_wiki.py` and committed |

**The CHANGELOG's "Installing and packaging" compatibility item** says, for someone coming from 0.1.5: CITAR installs from wheels for Windows x64, macOS 11+ (Apple silicon and Intel) and Linux x64 and arm64 (glibc 2.28+), Python 3.11 or newer; elsewhere `pip install citar` builds from the sdist with a Rust toolchain; `install.sh` and `install.ps1` install wheels only and `--worker`/`-Worker` install the helper; the Homebrew tap now ships only the helper (`brew uninstall citar`, then `install.sh` or pipx for the game, `brew install jprodgers/citar/citar-helper` for the helper); Scoop and winget gain a helper package; the Docker image is multi-arch and built from the wheels; the Windows installer upgrades in place (same AppId and data folder); release assets add the wheels, the packaged helper archives, `SHA256SUMS`, `THIRD-PARTY-LICENSES.txt` and attestations, and the helper's download names are unchanged; a self-hosted server deploys the wheel into the release layout, `citar-lab` and `requirements.txt` are gone, and the backup covers the whole state directory; release builds report their label (`v0.1.6`) in `citar doctor` and `citar-helper --version`.

**Checks:** `check_links.py --strict`, `mkdocs build --strict`, the wiki committed, and a stale-terms scan (`scripts/ci/stale_terms.py`, 5-12, run by test.yml's lint job) that refuses, outside the CHANGELOG's history and the Compatibility notes, the phrases of the old routes (the `git archive` deploy, `requirements.txt` installs, the game's Homebrew formula, `citar-lab`, `python -m citar.server` in a unit, the PyPI sdist in a manifest).

---

## P5.17 CI changes

| Workflow | Change |
|---|---|
| `package.yml` (new) | P5.3-P5.10: the wheels, the sdist and its build, the helpers and the manifest, the bundle and installer, the images, the install scripts, `assemble`, the package managers; `pull_request`, `push: main`, `workflow_dispatch`, `workflow_call` |
| `test.yml` | `workflow_call` with `mode`/`version`/`date`; `release.py prepare` first in every job; concurrency `tests-${{ github.workflow }}-${{ github.ref }}`; the Linux suite on **3.11, 3.12, 3.13 and 3.14** (the classifiers claim 3.14); `package`, `installer` and `docker` moved to package.yml; lint runs `release.py check`, `check_workflows.py` and (5-12) `stale_terms.py`; the `requirements.txt` step goes with the file (5-10) |
| `release.yml` | P5.11 |
| `rust.yml` | Its path filters gain `package.yml` (xtask reads it); lint runs `cargo xtask licenses --check` after `cargo fetch --locked` |
| `helper.yml` | Its `artifact` matrix stays equal to package.yml's `helpers` (the names test) |
| `determinism.yml`, `nightly.yml`, `docs.yml` | Unchanged; never dispatched by Phase 5 |

**`scripts/ci/check_workflows.py`** (5-02, extended in 5-03 and 5-09; PyYAML, which the dev extra brings through mkdocs and the lint job installs) proves, before any dispatch and on every pull request:
1. every workflow parses (PyYAML reads the `on` key as `True`; the script knows) and sets `permissions: contents: read` at the top;
2. a job **publishes** if it names an environment, asks for `id-token`, `pages`, `packages`, `contents`, `attestations`, `deployments` or `pull-requests` write, reads a secret other than `GITHUB_TOKEN`, uses a publishing action (`pypa/gh-action-pypi-publish`, `softprops/action-gh-release`, `actions/attest-build-provenance`, `actions/deploy-pages`, `docker/login-action`) or runs a publishing command (`gh release`, `twine upload`, `docker push`, `imagetools create`, `git push`, a mutating `gh api`); every publishing job is either in release.yml, carrying the tag guard as a conjunct of its `if:`, or in docs.yml, carrying 2-06b's guard to `refs/heads/main` (the site and the wiki, published from main only); neither guard may be weakened by `||`, `always()`, `!cancelled()` or `failure()`, and no other workflow publishes;
3. package.yml and test.yml publish nothing and name no environment and no secret;
4. a called workflow's concurrency group contains `github.workflow` and differs from its caller's;
5. no `run:` block interpolates `inputs.*`, `github.event.inputs.*` or `github.head_ref` (they reach shells through `env:`);
6. every maturin-action step pins `maturin-version: ${{ env.MATURIN_VERSION }}`;
7. every job of package.yml and test.yml runs `release.py prepare` after the checkout and before its first build or test step;
8. release.yml has no build step (cargo, maturin, PyInstaller, ISCC, `docker build`);
9. no upload path can include a `*.key` file or the throwaway key folder;
10. release.yml calls neither nightly.yml nor docs.yml.

`tests/test_workflows.py` runs it on the real files and on a planted fixture for each rule, each of which must fail naming its job.

---

## P5.18 Cross-cutting rules

### P5.18.1 Safety on this laptop (binding on every package)

P4.7.1, unchanged, with Phase 5's additions:
1. **Never** install a system service, a scheduled task or a startup entry on Windows, change a system or security setting (firewall, Defender, UAC, any registry value), or elevate to administrator. **Never install anything on Windows:** pip only into the package's own venv under `C:/dev/venv`; no Inno Setup, Docker, Podman, Scoop, winget package or Homebrew on Windows (those run on CI runners only; WSL's apt installs are rule 8's).
2. **Never** touch the owner's real CITAR data or configuration: every server, CLI, QA, smoke, bundle or install-script run uses its own data directory, database and configuration (`CITAR_DATA_DIR` and the state variables under the package's target folder), never `%LOCALAPPDATA%\CITAR`, the main checkout's `config/`, port 8799 or `C:/dev/citar-play`. Every helper run uses an isolated `--home`.
3. **Installer and install-script tests** run into temporary prefixes and never install into the owner's real Python, PATH, Start menu or registry: `install.ps1` only with `-Prefix`, `-BinDir` under the package's target folder and `-NoPath`; `install.sh` only with `HOME`, `--prefix` and `--bin-dir` in a temporary folder and `--no-profile`; the Inno installer is checked by a generated-script test locally and run only on a CI runner.
4. **Never** generate a release signing private key into the repository or commit any private key; `crates/citar-jobs/update-keys.pub` stays empty until the owner commits the public key; throwaway keys live in temporary folders outside any git working tree (`keygen` refuses otherwise); S12 scans.
5. **Never** connect a test helper to `citar.jimmieprodgers.com` or any non-local server; downloads in tests come from loopback servers (`CITAR_RELEASE_URL`).
6. **Never** load, unload or swap an LM Studio model or change its settings (Phase 5 has no real-model gate).
7. **Never touch the VPS:** no SSH, no copy, no deploy. The deploy is Phase 6's, with the owner's go-ahead.
8. **WSL** is `wsl -d Ubuntu-24.04 -u claude`; root (`-u root`) only inside WSL, where a gate needs it (apt installs, the staging run), each recorded in `ops/p5-notes.md`; never the WSL password; builds in `~/` from a `git bundle` or clone, never under `/mnt/c`; a server inside WSL binds `0.0.0.0` only after `wslinfo --networking-mode` says `nat`, else `127.0.0.1`; the staging run's system changes are removed by its `cleanup`, whose checks are S10's.
9. **The main checkout** is only read by implementers and reviewers; merge agents merge and push from it as in Phase 4, never staging, committing or reverting the owner's `.claude/launch.json`. Worktree agents never push.
10. **Never** push a tag, create a release, publish to PyPI or ghcr, push to `jprodgers/homebrew-citar` or `jprodgers/scoop-citar`, submit to winget, create an environment, a secret or a repository setting, add the `nightly` label to PR #5, or dispatch nightly.yml or docs.yml.

### P5.18.2 Dispatch rules

- **Who may dispatch release.yml:** the merge agent of 5-09 after its push (the first dry run); the merge agent of any later package that changed release.yml, package.yml or test.yml, after its push; and 5-13 (its implementation agent on the pushed head; its merge agent again after pushing any fix). Nobody else. test.yml's own `workflow_dispatch` may be used by the same agents when PR #5's run for the same head is missing or was cancelled.
- **Only** `gh workflow run release.yml --ref v0.1.6 -f version=0.1.6` (the owner's version number if they choose another), on the **pushed** `v0.1.6`; never a tag ref, never another branch.
- **Before every dispatch:** S8 at the pushed head (`check_workflows.py` green: no publishing job can run outside a tag push); `gh api repos/jprodgers/CITAR/environments --jq '[.environments[].name]'` is exactly `["github-pages","pypi"]`; `git ls-remote --tags origin` and `gh release list` recorded.
- **Watched to the end:** `gh run watch <id> --exit-status`; afterwards the same three readings, unchanged (a new environment, tag or release stops everything and is reported to the owner).
- **Bounded:** at most three fix-and-push rounds per dispatching agent, each fix through the local gates first; after the third failure the agent stops and reports the failing job's log.
- **package.yml at a merge push** runs by itself (PR #5's `pull_request` event): the merge agents of 5-02 to 5-12 watch that run and test.yml's for their push (`gh run list --workflow package.yml --branch v0.1.6 --event pull_request`), with the same three-round bound. That is not a dispatch.

### P5.18.3 Disk

- Every package builds with `CARGO_TARGET_DIR=C:/dev/target/citar-p-5-xx` and its venv `C:/dev/venv/p-5-xx`; maturin's `--out`, PyInstaller's `--distpath`/`--workpath`, sdist builds and podman contexts are under that target folder. **Nothing builds into the OneDrive checkout** (no `target/`, `dist/`, `build/` or `installer/output/` there; `git status --ignored` checked at the end).
- **The merge step deletes** the package's target folder and venv after the push; reviewers reuse the package's own target folder and never make another.
- **WSL:** one clone per package (`~/p5/p-5-xx`, removed at its merge) and one shared target `~/target/citar-p5`; podman's images pruned at each package's end except the base images (`python:3.12-slim-trixie`, the maturin container), which 5-13 prunes too. The Phase 4 WSL targets (`~/target/citar-p-4-*`), the old `C:/dev/venv/p-2-*` to `p-4-*` venvs and the stray `target/` in the main checkout are the orchestrator's or owner's to remove; packages leave them alone.

### P5.18.4 CI parity and CI-only gates

- **Locally:** Windows x64 natively (the Windows wheel, bundle, helper, `install.ps1`), Linux x64 in WSL (the manylinux_2_28 x86_64 wheel through the maturin container under podman, the image under podman, `install.sh`, the deploy and the staging), Python 3.11, 3.12 and 3.14 on Windows, 3.12 in WSL, `--locked` everywhere.
- **CI-only, named in each package:** macOS arm64 and x64, Linux arm64, Inno Setup's compile and the silent install, Scoop, winget, Homebrew, Docker's buildx and both native images, the attestation and publishing steps (tag-only, never run in Phase 5).
- **Nothing only release.yml catches:** release.yml builds nothing; its builds are package.yml's, which runs on every pull request; the version checks run in test.yml's lint; the tag-only jobs are the only release-only code and are proven by `check_workflows.py` and the dry run's summary.

### P5.18.5 The standard gates

Every package's gates include, besides its own (P4.7.5's, renumbered where Phase 5 adds):
- **S1** the Python suite (`python -m unittest discover -s tests`) in the package's venv, with `cargo xtask develop` when Rust changed; its count recorded. **S1b** the suite on Python 3.12 under `TZ=UTC0` (`C:/dev/venv/py312-check` with `CITAR_EXT_DIR` at the package's extension). From 5-02, **S1c** the suite on Windows' Python 3.14 when the package changes Python.
- **S2** ruff (and `sync_requirements.py --check` until 5-10 retires it). **S3** `audit_routes.py --strict`. **S4** `check_links.py --strict`. **S5** when `docs/` changed: `mkdocs build --strict` and `build_wiki.py` with `wiki/` committed. **S6** the front-end checks.
- **S7** when Rust (xtask included) changed: fmt, clippy `-D warnings`, nextest, the doctests, `cargo xtask check`, the refcheck run, `cargo golden check`, the `jobs` set in `release`.
- **S8** when a workflow or a script one runs changed: every workflow parses; `scripts/ci/check_workflows.py` (from 5-02); `cargo xtask check`; `node --test citar/web/tests/helper.test.mjs`.
- **S9** the package's Linux half in WSL, as its gates name it (Rust in WSL when citar-helper or citar-jobs changed).
- **S10** the owner's data and the machine are as they were, before and after: P4.7.5's list (the two `servers.json` hashes, `citar.db` and `secret_key` mtimes, `helper.json`, machine keys, `config/worker.json`, no helper, job-host or server process, no `citar-helper` service, Run value or task, no `CITAR_*` variable, the main checkout showing only `.claude/launch.json`), plus: the user's PATH value (HKCU `Environment`) hashed unchanged; no `%LOCALAPPDATA%\CITAR\app` or `\bin`; no `{9C1D2A54-...}_is1` uninstall key; no Start menu `CITAR` folder; in WSL no `~/.local/bin/citar*` or `~/.local/share/citar-app` for `claude`, no `citar` user, `/opt/citar*`, `/var/lib/citar*`, `/etc/citar`, citar unit or staging slice, nginx (once a package has installed it) stopped and disabled, no running container; on GitHub the environments, tags and releases unchanged.
- **S11** `python scripts/release.py check` (from 5-01).
- **S12** where signing is touched: no private key in the tree, the ignored files or the package's history (Phase 4's key scan), `update-keys.pub` 0 bytes.
- **CI-only** gates as named per package; nobody dispatches anything outside P5.18.2.

CI timing lessons stand: speed budgets times three under `CI`, durations by `perf_counter`, `TZ=UTC0`, `fsutil.newest_first`, waits on the condition with a deadline (never sleep-then-assert), Windows runners elevated as `runneradmin`, macOS limits soft.

### P5.18.6 Process

As Phase 4: a worktree per package (`.claude/worktrees/p-5-xx`, branch `p-5-xx`, from the previous package's merge); `CARGO_TARGET_DIR` before every cargo or maturin command; files written with the Write tool (Git Bash mangles backslashes in heredocs); small commits ending with the Co-Authored-By line; never `ops/`, `saves/` or `.claude/launch.json`. Each package ends with its gates, a review and a fix round, its as-built note with its start and end times, and handoff notes in `ops/p5-notes.md` (gitignored), apt installs in WSL included.

---

## P5.19 Decisions taken in this design

1. **One release build, `package.yml`,** run on every pull request and called by release.yml, which builds nothing itself.
2. **Cargo's version is the source;** `release.py` checks and bumps the rest; a dry run bumps its runner's checkout, so it builds exactly what a tag would.
3. **The label** is `v<version>` on a tag, `v<version>-dryrun+<sha>` on a dry run, `ci+<sha>` otherwise, on every release build.
4. **Wheels are built natively** on five runners with maturin 1.15.0: abi3-py311, manylinux_2_28, macOS 11; each smoke-tested on its runner on 3.11 and 3.14, and all five must report one build and play one seeded lab job to one result hash.
5. **The sdist is built into a wheel as a user would** and smoke-tested.
6. **The release's tested dependency set is an asset** (`server-constraints`), used by the image and the VPS.
7. **The helper ships twice:** unversioned self-updating binaries (the download links, the manifest, `--worker`) and versioned `packaged` archives (Homebrew, Scoop, winget).
8. **A dry run proves the signing path with a throwaway key** and two verifiers; the real key signs only a tag, under the environment's reviewer, and must match a committed public key.
9. **Every publishing job is tag-push-only** (`event_name == 'push'` and a `refs/tags/v` ref), proven by `check_workflows.py`.
10. **Attestations only for a tag;** the dry run shows their subjects.
11. **The image pushed is the image tested** (saved, loaded, pushed by architecture, joined); both architectures built natively on `python:3.12-slim-trixie`.
12. **The GitHub release is a draft until whole,** then published, so `latest` never lacks a helper.
13. **The install scripts install wheels only** and say so on an unsupported platform; `--worker` installs the self-updating helper with its checksum.
14. **The Homebrew tap ships only `citar-helper`;** the game's formula is deleted.
15. **Third-party notices** are generated by xtask and travel with every binary.
16. **The server runs from a release layout** (`/opt/citar/releases/<v>`, `current`), units in the state directory, one unit definition, no `citar-lab`, no `requirements.txt`.
17. **`citar-deploy` scripts install, cutover, rollback and uncutover,** each with `--plan`; the laptop fetches and verifies the wheel; the box's other sites are checked before and after, and only CITAR's own failure rolls back automatically.
18. **The staging run** reproduces the VPS's layout and limits in WSL, including the cutover from 0.1.5, and Phase 6 repeats it before the real deploy.
19. **The bundle's job host and the MCP command** are fixed for installed copies; `doctor` proves the job host everywhere.
20. **The Linux suite runs 3.14 too.**

## P5.20 Risks

1. **CI-only platforms fail late.** macOS, arm64, Inno, Scoop, winget and Homebrew are first seen at a merge push. Mitigation: package.yml on PR #5 at every merge push, watched; each package's local stand-ins; the three-round bound with a report.
2. **The runners change** (Inno Setup absent from an image, winget missing, a macOS image retired). Mitigation: each tool found or installed in its job, its version printed; a missing optional check (winget install) reported, never silently skipped.
3. **A release run partly publishes.** Mitigation: signing first, PyPI's `skip-existing` for a re-run, the release a draft until whole, the image tags made last; RELEASING.md says what to re-run.
4. **The VPS's memory.** The server and its lab now share 400M. Mitigation: measured in staging under the box's limits; the host's settings and the unit's limits set from the measurement; `OOMScoreAdjust` keeps the other sites safe.
5. **Dependency drift between the test and the deploy.** Mitigation: the constraints asset.
6. **The environment rule.** A skipped job is not expected to create its environment; every dispatch reads the environments before and after, and a change stops the phase.
7. **The WSL disk** grows with images and targets. Mitigation: one target, prunes, the exit's prune.
8. **winget** may never merge. Mitigation: nothing depends on it; the other routes cover Windows.

## P5.21 Questions for the owner, and what was settled without asking

**Two questions** (neither blocks a package: the design takes the default and makes the other answer one argument):
1. **The release number:** 0.1.6, as the branch and every design say, or 0.2.0 (plan §8's aside: "It might deserve the number 0.2.0")? Default 0.1.6: the dry runs use `-f version=0.1.6`; another number changes that input and the bump's argument, nothing else.
2. **The update-signing key before the 0.1.6 tag?** A helper trusts only the keys it was built with, so if 0.1.6 ships with `update-keys.pub` empty, no helper installed from 0.1.6 can ever update itself: each must be replaced by hand at 0.1.7. Default: the release proceeds either way (P5.4.2's first rule), and RELEASING.md's step 1 asks the owner to make `helper-signing` and commit the public key before tagging.

**Settled without asking** (the plan, the owner's standing rules or common sense decide): the old Homebrew `citar` formula is deleted rather than deprecated (no other users; the CHANGELOG says how to move); Scoop and winget carry the helper as packaged builds; winget's manifests are still generated, validated and submitted in Phase 6 though Microsoft has merged none (minutes of work, no dependency on it); attestations only for a tag (a dry run would publish to a public log); the image moves to Debian 13 (Debian 12 is past regular security support); the Linux suite gains 3.14 (the classifiers claim it); release.yml does not call rust.yml or determinism.yml (Phase 6's bump pull request runs every workflow before the tag); the GitHub release is a draft until whole; the VPS's memory limits come from the staging measurement, not a guess.

## P5.22 Work breakdown at a glance

Thirteen packages, strictly in this order, one agent at a time. Each ends with its gates, a review and a fix round, and its as-built note.

| # | Package | Plan §7 Phase 5 part |
|---|---|---|
| 5-01 | Versions, the build label and `release.py` | builds (release prerequisites) |
| 5-02 | `package.yml`: the wheel matrix, the sdist, the workflow guard | builds |
| 5-03 | The helper's release builds and the signing path | builds |
| 5-04 | Third-party notices | builds |
| 5-05 | The Windows bundle and installer (and the installed copies' fixes) | builds |
| 5-06 | The image from wheels, both architectures | builds |
| 5-07 | The install scripts: wheels only, the helper download | builds |
| 5-08 | Package managers and assembling a release | builds |
| 5-09 | release.yml end to end, the checklist, the first dry run | builds, exit |
| 5-10 | The deploy from the release wheel | VPS |
| 5-11 | The staging dry run in WSL | VPS |
| 5-12 | The docs pass | docs |
| 5-13 | Phase 5 exit | exit |

The order: the version and label first, because every build uses them; the wheels before anything built from them (the bundle, the image, the install scripts, the deploy); the helper and notices before the package managers that ship them; `assemble` (5-08) before release.yml (5-09), which only publishes what package.yml assembled; the deploy after the release's asset names are fixed; the docs pass when everything it describes exists; the exit last.

---

## Appendix F: Phase 5 work breakdown

Each package is sized for one implementation session (a Phase 4 package or smaller), starts from the previous package's merge, and ends with its gates passing, then a review and a fix round. The standard gates S1-S12 (P5.18.5) are named where they apply; every other gate is local and can be rerun by a reviewer, and CI-only gates are named as such, with the agent that proves them (P5.18.2). Every package keeps the safety rules of P5.18.1.

### 5-01: Versions, the build label and `release.py`

- **Depends on:** nothing (`v0.1.6` at `51ce329`)
- **Estimated size:** ~450 Python (`scripts/release.py`) + ~350 tests + small edits
- **Scope:** P5.2: `scripts/release.py` (`check`, `bump`, `prepare`, `label`, `meta`, `notes`, `assets`) over the checked set (Cargo.toml, the lock's eleven members, the fuzz lock's three, `citar/__init__.py`, `installer/citar.iss`'s `AppVersion` with a new `AppVersionNumeric`, KNOWN_ISSUES' header, the CHANGELOG); `citar.iss`'s comment and `docs/INSTALL.md:33` made version-free; `tests/test_release_script.py`; test.yml's lint job runs `release.py check`; CONTRIBUTING's "Releasing" names `release.py bump` (RELEASING.md is 5-09's).
- **Gates:**
  1. `release.py check` passes on the tree; each place planted out of step, one at a time, fails naming its file and line.
  2. In a scratch clone (`C:/dev/target/citar-p-5-01/clone`): `bump 0.1.6 --date 2026-11-01` moves every place and nothing else (`git diff --stat` recorded); `cargo metadata --locked` accepts the lock, and rust.yml's `cargo check --locked` of the fuzz crate accepts its lock; `cargo xtask check` passes; `check --version 0.1.6` passes; the CHANGELOG has `## [0.1.6] - 2026-11-01`, a new empty Unreleased and both compare links; a second bump to 0.1.6 and a bump to 0.1.4 are refused.
  3. `prepare`: `--mode tag --version 0.1.5` passes at the head and `--version 0.1.6` fails; `--mode dry-run --version 0.1.6` bumps the scratch clone and passes; `--mode ci` only checks.
  4. `notes 0.1.5` equals what release.yml's awk prints from the same file, byte for byte; `label` and `meta` for each mode (a bad version, `0.1.6;rm`, refused); `assets 0.1.6` and `assets 0.1.6-rc.1` equal P5.10's list (PEP 440 names `0.1.6rc1`).
  5. **The bump moves no committed answer:** in the scratch clone after the bump, `cargo nextest run -p citar-jobs --test golden` (the build id moved; no bless), `cargo golden check`, and the Python modules that read versions or builds (`test_engine_module`, `test_jobs*`, `test_helper_updates`, `test_ci_scripts`) pass, with `cargo xtask develop` into the package's target.
  6. S1, S1b, S2-S4, S8, S10, S11.

### 5-02: `package.yml`: the wheel matrix, the sdist, the workflow guard

- **Depends on:** 5-01
- **Estimated size:** ~300 workflow YAML + ~450 Python (`check_workflows.py`, `smoke_wheel.py`) + ~350 tests + check_dist edits
- **Scope:** P5.3 and P5.17's first half: `.github/workflows/package.yml` with `meta`, `wheels` (P5.3.2's table, `CITAR_BUILD_ID`, check_dist per tag), the smoke on 3.11 and 3.14 per runner (`scripts/ci/smoke_wheel.py`, P5.3.4, the job host's `ready` and a seeded lab job's result hash), the constraints file, `sdist` and `sdist-build`; test.yml's `package`, `installer` and `docker` jobs moved onto the release wheels; test.yml's `workflow_call` inputs, `prepare` in every job, the concurrency group, the 3.14 leg; `scripts/ci/check_workflows.py` (rules 1-7 and 9-10 of P5.17; rule 8 comes with 5-09's release.yml) and `tests/test_workflows.py`; the old release.yml's publishing jobs given the full tag guard (the event check added to `publish-pypi`, `publish-image` and `github-release`, and a guard for `sign-helper-manifest`), so the check passes while release.yml stays gated until 5-09; `cargo xtask check` reads package.yml (the helper rule) and rust.yml's filters list it (the triggers rule); WSL: podman, `python3-venv` and `python3.12-venv` installed as root (recorded).
- **Gates:**
  1. **Windows:** the release wheel built with maturin 1.15.0 (`--release --locked --out` under the target, `CITAR_BUILD_ID` set) passes `check_dist.py` as `cp311-abi3-win_amd64` with `_engine.pyd`; `smoke_wheel.py` passes in fresh venvs on 3.11 and 3.14 (label equal, job host verifiable, the lab job's result hash recorded).
  2. **WSL:** the manylinux_2_28 x86_64 wheel built in the maturin container (`ghcr.io/pyo3/maturin` at `MATURIN_VERSION`, `--compatibility manylinux_2_28`) under podman passes `check_dist.py` with its tag; the smoke passes on 3.12; the lab job's result hash equals Windows'. The constraints file is written from a `[server]` venv.
  3. The sdist passes `check_dist.py sdist`, and a wheel built from it in WSL passes the smoke with `--label unknown`.
  4. `check_workflows.py` passes on the tree; each planted fixture fails naming its job: a publishing step without the guard; the guard without the event check; a guard under `always()`; an environment named in package.yml; a called workflow's group without `github.workflow`; `${{ inputs.version }}` in a `run:`; a maturin step without the pin; a build before `prepare`; an upload of `*.key`.
  5. Every workflow parses; `cargo xtask check` passes, and fails (`[triggers]`) with package.yml taken out of rust.yml's filters.
  6. **S1c:** the suite on Windows' Python 3.14 passes (before the CI leg is added).
  7. **CI-only, at the merge push (PR #5's runs, watched by the merge agent):** the five wheels on their runners, the smokes on 3.11 and 3.14, `sdist-build`, the moved installer and docker jobs, test.yml's 3.14 leg.
  8. S1, S1b, S2-S4, S6, S7, S8, S9 (gates 2-3), S10, S11.

### 5-03: The helper's release builds and the signing path

- **Depends on:** 5-02
- **Estimated size:** ~200 Rust (`cargo xtask verify-manifest` and its tests) + ~220 workflow YAML + ~200 Python (`verify_manifest.py`, tests) + the names test
- **Scope:** P5.4: package.yml's `helpers` matrix (the self-updating build under its unversioned name and the packaged build in its versioned archive, each run with `--version` and `--self-test`, the packaged one's `update check`; the Fedora TLS check; macOS deployment target; the label) and `helper-manifest` job (manifest, throwaway key in `$RUNNER_TEMP`, signature, both verifiers, the one-byte negative, the `update-keys.pub` check, the two artifacts); `xtask verify-manifest`; `scripts/ci/verify_manifest.py`; release.yml's old `build-helper` and `sign-helper-manifest` jobs removed (release.yml stays gated until 5-09); `helper.test.mjs` reads package.yml's `helpers` matrix and helper.yml's `artifact`; check_workflows rule 9 covers the key folder; the archives carry `LICENSE` and `NOTICE.md` (5-04 adds the third file).
- **Gates:**
  1. **Windows:** both builds of `x86_64-pc-windows-msvc` in `release` with the label: `--version` names V, the label and the target; `--self-test` passes; the packaged build's `update check` says packaged and `notify`, and `updates = "auto"` in its `helper.json` is refused; `cargo nextest run -p citar-helper --features packaged` passes.
  2. **WSL:** both linux-x64 builds; the archive's listing is exactly its four files (three until 5-04); `--self-test`.
  3. **Signing, in WSL:** with the linux-x64 build and stand-in copies for the other four names, `helper-manifest` writes the manifest; a key from `keygen` in `/tmp`; `sign-manifest`; `verify-manifest` passes with its public key, fails with another key and on a manifest with one byte changed; `verify_manifest.py` agrees in all three; the release build (no test features) with the empty `update-keys.pub` reports updates unavailable.
  4. **Names:** `helper.test.mjs` passes reading package.yml; a sixth name planted in the matrix, or a matrix line out of step with helper.yml, fails.
  5. `check_workflows.py`: package.yml names no environment or secret; an upload path under `$RUNNER_TEMP/throwaway` is refused.
  6. **CI-only, at the merge push:** the ten builds on five runners and the `helper-manifest` job.
  7. S1, S2-S4, S6, S7, S8, S9 (gates 2-3), S10, S11, S12.

### 5-04: Third-party notices

- **Depends on:** 5-03
- **Estimated size:** ~400 Rust (`cargo xtask licenses`) + the generated file + small pyproject, workflow and NOTICE edits
- **Scope:** P5.5: `cargo xtask licenses [--check]` (the closure of `citar-py` and `citar-helper` over the release targets from `cargo metadata --locked`, the licence texts from the registry sources, the allow-list), `packaging/THIRD-PARTY-LICENSES.txt` committed; rust.yml's lint runs `--check` after `cargo fetch --locked`; `pyproject.toml`'s `license-files` and `check_dist.py`'s expectations; the packaged archives and the release carry it; NOTICE.md's section.
- **Gates:**
  1. Generated twice on Windows and once in WSL: identical bytes; every crate of the closure appears once with its expression and at least one licence text.
  2. A copy of the metadata with one crate's licence changed to `GPL-3.0-only` is refused naming the crate; a lock with one version changed makes `--check` fail.
  3. The Windows release wheel carries the file in its dist-info (`check_dist.py` passes; `pip show -f` lists it after install).
  4. **CI-only, at the merge push:** rust.yml's `--check`; the archives' listings include the file.
  5. S4, S7, S8, S9 (gate 1 in WSL), S10, S11.

### 5-05: The Windows bundle and installer, and the installed copies' fixes

- **Depends on:** 5-04
- **Estimated size:** ~200 Python (the frozen job host, `jobs-host`, doctor's jobs line, `/api/meta`) + ~40 JS + ~120 spec + ~200 workflow YAML + ~300 tests
- **Scope:** P5.6: `installer/citar.spec` from the installed wheel; `citar/jobs/host.py`'s frozen command and the hidden `jobs-host` command; `citar doctor`'s jobs line; `/api/meta`'s `version`, `label`, `mcp_command` and no paths in server mode, and `lobby.js`; `installer/citar.iss` (`AppVersionNumeric`); package.yml's `windows` job (the venv from the wheel, PyInstaller, the bundle checks, `PYTHON-LICENSES.txt`, ISCC found or installed, the silent per-user install and uninstall on the runner, the zip); `tests/test_installer_script.py` (the .iss: per user, the Files source equals the spec's `COLLECT` name, `OutputBaseFilename`, the PATH task unchecked, the uninstall prompt's default No, `VersionInfoVersion` from the numeric define).
- **Gates:**
  1. **The bundle, locally:** the 5-02 Windows wheel in a fresh 3.12 venv (`C:/dev/venv/p-5-05-bundle`, pip only into it) with PyInstaller; the bundle under the package's target; `citar.exe --version` (V), `where`, `doctor --quiet` exit 0 with every state variable under the target, its jobs line `verifiable`; `_engine.pyd` in the bundle; the build analysed no file of the checkout's `citar/` (the spec's assertion).
  2. **The bundle serves and plays its lab:** `citar.exe serve` on a free loopback port (never 8799) with an isolated data directory; `/api/meta` names the bundle's `citar.exe mcp` and no path in server mode; a lab experiment of one duel game is played by the bundle's job host (a results line appears); stopped, no process left.
  3. **A gate that can fail:** the old host command put back in a bundle makes the jobs line and gate 2's lab fail.
  4. Tests: the host command per kind (checkout, wheel, frozen); `/api/meta` per mode; the lobby's command (`node --test`); the .iss test.
  5. **CI-only, at the merge push:** package.yml's `windows` job: the compile, the silent install, run and uninstall, the zip.
  6. S1, S1b, S1c, S2-S6, S8, S10 (no Start menu folder, no uninstall key, PATH unchanged), S11.

### 5-06: The image from wheels, both architectures

- **Depends on:** 5-05
- **Estimated size:** ~80 Dockerfile + ~150 workflow YAML + ~100 checks
- **Scope:** P5.7: the Dockerfile (trixie base, constraints, `--only-binary`), package.yml's `image` matrix (native amd64 and arm64, each from its wheel, run and checked, saved on dry runs and tags), `docker compose config`; test.yml's moved `docker` job replaced; DEPLOY.md's Docker lines.
- **Gates:**
  1. **WSL, podman:** the image built from 5-02's local manylinux wheel and constraints; run with its port on `127.0.0.1`: the health route within 60 s, no test operations, version and label, `citar doctor --quiet` with the job host verifiable as uid 10001, a restart keeping the volume's state; the image saved and loaded again with the same id.
  2. **CI-only, at the merge push:** both architectures built natively and run; `docker compose config`.
  3. S2, S4, S8, S9 (gate 1), S10, S11.

### 5-07: The install scripts: wheels only, the helper download

- **Depends on:** 5-06
- **Estimated size:** ~250 bash + ~200 PowerShell + ~180 workflow YAML + ~250 tests
- **Scope:** P5.8: `install.sh` and `install.ps1` (`--only-binary=citar`, the platform check and its message, `--prefix`/`-Prefix`, `--bin-dir`/`-BinDir`, `--no-profile`, `-NoPath`, `--from`/`-From` with a file or folder, `--worker`/`-Worker` downloading the self-updating helper with its checksum, `CITAR_RELEASE_URL` with the loopback-only `http` rule, uninstall within the given prefix); package.yml's `install` matrix (ubuntu x64 and arm64, macOS arm64 and x64, Windows) against the built wheels and helpers on a loopback server; a local wrapper test for `install.ps1`.
- **Gates:**
  1. **WSL, as `claude`, everything in a temporary `HOME`:** `install.sh --from <local manylinux wheel> --prefix … --bin-dir … --no-profile --no-setup` installs a working `citar`; a folder holding only an sdist is refused with the no-wheel message; the platform check's message for musl and an unknown architecture (through its test hook); `--worker` with `CITAR_RELEASE_URL=http://127.0.0.1:<port>` serving the WSL helper build and its `.sha256` installs a helper that runs, a wrong checksum is refused before anything is written, and an `http` URL to a non-loopback host is refused; `--uninstall` leaves the prefix and bin folder empty.
  2. **Windows:** `install.ps1 -Prefix … -BinDir … -NoPath -From <local win wheel> -NoSetup` installs a working `citar`; `-Worker` against a loopback server installs the Windows helper; the user's PATH value is byte-identical before and after; `-Uninstall` with the same prefix removes only it; the wrapper refuses a run without `-Prefix` under the target or without `-NoPath`.
  3. **CI-only, at the merge push:** the `install` matrix on five platforms (PATH changes allowed on runners).
  4. S1 (the tests), S2, S4, S8, S9 (gate 1), S10, S11.

### 5-08: Package managers and assembling a release

- **Depends on:** 5-07
- **Estimated size:** ~450 Python (`release_checksums.py`, `assemble_release.py`) + the templates + ~250 workflow YAML + ~300 tests
- **Scope:** P5.9 and P5.10: `packaging/` templates (Scoop `citar` and `citar-helper`, winget `game/` and `helper/`, Homebrew `citar-helper.rb` with its service block), `packaging/homebrew/citar.rb` deleted; `scripts/release_checksums.py` rewritten (`--from-dir`, `--base-url`, `--out`, `--write`, `--verify`); `scripts/ci/assemble_release.py` and package.yml's `assemble` job (the list, one build across every artifact, one result hash across the wheels, `SHA256SUMS`, the notes, the manifests, the three artifacts); the `managers` job (Scoop and winget on Windows, Homebrew on macOS arm64, macOS x64 and Linux x64); `packaging/README.md` rewritten.
- **Gates:**
  1. `assemble_release.py` on a fake artifact tree: the complete set passes and writes `SHA256SUMS` that `sha256sum -c` accepts; a missing asset, an extra one, a helper reporting another build, a wheel reporting another jobs build, and a wheel with another result hash each fail naming the file.
  2. The generator over the package's local artifacts (stand-ins for the CI-only files) writes every manifest: each URL and hash right; the Scoop JSON has its keys and parses; each winget file starts with its schema header and has the required fields (`Documentations` spelled so); the formula has a URL and SHA-256 under each of the four platform blocks, and `ruby -c` accepts it in WSL (ruby installed as root, recorded).
  3. **CI-only, at the merge push:** `assemble` and `managers`: `scoop install` of both packages from loopback, `winget validate` (and `install --manifest` where present), the schema validation, `brew install`, `brew test`, `brew audit --strict` and `brew style` on three platforms.
  4. S1 (the tests), S2, S4, S8, S9 (gate 2's ruby), S10, S11.

### 5-09: release.yml end to end, the checklist, the first release dry run

- **Depends on:** 5-08
- **Estimated size:** ~260 workflow YAML + ~200 Python (check_workflows' release rules, `scripts/ci/release_sign.py`, `scripts/release/managers.sh` with its tests) + `packaging/RELEASING.md` (~250 lines)
- **Scope:** P5.11, P5.12, P5.15: release.yml rewritten (the gate removed; `meta`, `tests`, `package`; the tag-only `sign-helper-manifest` with P5.4.2's four rules in `scripts/ci/release_sign.py`, `publish-pypi`, `publish-image`, `attest`, `github-release` as a draft then published; the dry-run-only `dry-run-summary`; concurrency; per-job permissions); check_workflows rule 8 and the release-specific checks (only release.yml's tag jobs publish; the publishing order of P5.11); `packaging/RELEASING.md` (P5.15) and `scripts/release/managers.sh` (clones into a temporary folder, copies, shows the diff, pushes only with `--push`); CONTRIBUTING's "Releasing".
- **Gates:**
  1. `check_workflows.py` passes; planted: a publishing job guarded by `startsWith(github.ref, 'refs/tags/')` alone (a dispatch on a tag would publish), `publish-pypi` not needing the signature, a `docker build` step in release.yml, `github-release` without `draft` first: each fails naming its job.
  2. `release_sign.py` with throwaway keys in a temporary folder: the four states of P5.4.2 give their four outcomes (a notice, three refusals naming the cause, a signature that verifies against the committed line).
  3. The release's upload list equals `release.py assets V` (and the `.sig` only on a signed tag); `managers.sh 0.1.6 --plan` against local throwaway clones shows the diff and pushes nothing.
  4. S2, S4, S8, S10, S11, S12.
  5. **The first release dry run (CI-only), by this package's merge agent after its push** (P5.18.2): environments, tags and releases read; `gh workflow run release.yml --ref v0.1.6 -f version=0.1.6`; `gh run watch --exit-status`; `meta`, `tests`, `package` and `dry-run-summary` green, every tag-only job skipped; the readings unchanged; the `release-preview` artifact's list equals `release.py assets 0.1.6`; at most three fix rounds; the run id and the jobs' conclusions in the merge's notes.

### 5-10: The deploy from the release wheel

- **Depends on:** 5-09
- **Estimated size:** ~500 bash (`citar-deploy.sh`, `citar-admin.sh`, `citar-backup.sh`) + ~200 Python (`scripts/deploy/bundle.py`) + ~80 Python (the wizard) + units + ~350 tests + docs
- **Scope:** P5.13: the release layout; `deploy/citar.service`, the backup, the admin wrapper; `deploy/citar-deploy.sh` (`install`, `cutover`, `rollback`, `uncutover`, `status`, `prune`, `--plan`); `scripts/deploy/bundle.py` (`gh release download`, hashes, `gh attestation verify`, `git archive` of `deploy/` and `env.example`; `--from-dir`); `install.sh --server` and `citar setup --server` on the layout, the wizard's unit equal to `deploy/citar.service` (a test); `requirements.txt`, `scripts/sync_requirements.py` and test.yml's step retired; DEPLOY.md, RUNBOOK.md ("Deploying a new version", rollback, restores) and VPS.md's deploy parts.
- **Gates:**
  1. `shellcheck` on every script in `deploy/` and `scripts/deploy/` (installed in WSL as root, recorded); `systemd-analyze verify` of the units in WSL.
  2. `--plan` of `install`, `cutover`, `rollback`, `uncutover` and `prune` against stand-in bundles prints exactly the committed expected texts (`tests/data/deploy/*.txt`, LF-pinned); a bundle with a changed byte, a wheel of another platform, too little disk and a failing other site in the preflight are each refused before any change, naming why.
  3. `bundle.py --from-dir` makes a bundle whose `SHA256SUMS` checks; a tampered wheel is refused; the published path is exercised with a stubbed `gh` (the arguments of `release download` and `attestation verify`).
  4. The wizard's rendered unit and `deploy/citar.service` hold the same `[Service]` directives; `install.sh --server --plan` names the layout.
  5. S1, S1b, S2 (ruff alone from here), S3-S5, S8 (test.yml), S9 (gate 1), S10, S11.

### 5-11: The staging dry run in WSL

- **Depends on:** 5-10
- **Estimated size:** ~450 bash (`scripts/deploy/staging.sh`) + ~250 Python (`scripts/deploy/staging_check.py`) + RUNBOOK's staging section
- **Scope:** P5.14 with local wheels: the slice, the stand-in sites, the 0.1.5 layout, `cutover`, the game and the lab under the sandbox, the measurements, `install` of a second version, `rollback`, the backup and restore drill, `uncutover`, `cleanup`; two `-rc` wheels built locally in the maturin container from bumped scratch clones (`0.1.6-rc.1`, `0.1.6-rc.2`).
- **Gates:**
  1. `staging.sh setup` then `run` exit 0, every step's check recorded: the stand-in sites 200 before and after each step; the cutover's health within 60 s; the game's 40 rounds and the lab's three games played (the job host under `citar.service`, `RLIMIT_DATA` fallback logged); `oom_kill` 0 in the slice; the upgrade, the rollback and the second upgrade each healthy with the right version on `/api/meta`; the restore drill reads its accounts; `uncutover` brings 0.1.5 back and it answers.
  2. **A gate that can fail:** a planted bad release (a wheel whose server exits at start) makes `install` roll back by itself and exit 1, with the previous release serving.
  3. The measurements (idle, game, lab, pip install, seconds a round) in the as-built note with the recommended `MemoryMax`, `MemoryHigh` and host memory figure.
  4. `cleanup` leaves S10's WSL items clean.
  5. S2, S4, S5 (RUNBOOK), S10, S11.

### 5-12: The docs pass

- **Depends on:** 5-11
- **Estimated size:** docs (~1,500 lines touched) + `scripts/ci/stale_terms.py` (~120) + tests
- **Scope:** P5.16: every file of its table; the CHANGELOG's compatibility item, Added and Fixed; KNOWN_ISSUES; `stale_terms.py` in test.yml's lint; the wiki regenerated.
- **Gates:**
  1. Every file in P5.16's table changed, or named in the as-built note as read with nothing to change, with the line that was checked.
  2. `stale_terms.py` passes; each of its phrases planted in a doc fails naming the file and line; the CHANGELOG's history is exempt.
  3. S4, S5 (`mkdocs build --strict`, the wiki committed and unchanged by a second `build_wiki.py`), S1 (the script's tests), S8, S10, S11.

### 5-13: Phase 5 exit

- **Depends on:** 5-12
- **Estimated size:** evidence, scripts' runs, small fixes; "As built in 5-13"
- **Scope:** P5.1.4 measured row by row; the dry run on the pushed `v0.1.6` (P5.18.2); the staging run with the dry run's CI-built linux-x64 wheel and constraints (`gh run download`, then `bundle.py --from-dir`) under the VPS's limits; the bump rehearsal; RELEASING.md walked through in its dry-run forms (`release.py bump` in a scratch clone, `release_checksums.py --from-dir` over the dry run's `release-assets`, `managers.sh --plan`, `bundle.py --from-dir`, `citar-deploy --plan`); WSL's podman images pruned; the Phase 6 handoff in `ops/p5-notes.md`.
- **Gates:**
  1. **The release dry run is green on every platform** (CI-only, dispatched by this package on the pushed head; any fix goes through its merge agent, which dispatches again, three rounds at most): `meta`, test.yml's every leg, and package.yml's every job (the five wheels and their smokes on 3.11 and 3.14, the sdist and its build, the bundle and installer with the silent install, the ten helpers and the manifest with its throwaway signature verified, both images built natively and run, the install scripts on five platforms, `assemble` with one build and one result hash, the package managers), every tag-only job skipped, the environments, tags and releases unchanged. The run id, each job's conclusion and duration, and the `release-preview` contents recorded.
  2. **The staging deploy from the CI-built wheel:** `staging.sh` with a bundle made from the dry run's artifacts for the cutover, and a `0.1.7-rc.1` wheel built locally as 5-11 builds its own (5-11's are gone with its target folder) for the upgrade and the rollback; every check of 5-11's gate 1 again, the measurements again, compared with 5-11's.
  3. **The bump rehearsal:** in a scratch clone, `release.py bump 0.1.6`; `cargo xtask develop`; S1 and S7's nextest, `golden check` and `xtask check` green there; the clone deleted.
  4. **The checklist's dry runs:** each step of RELEASING.md that has one run in that form, its output recorded; the manifests from the dry run's assets equal the `release-preview` artifact's.
  5. The key scan (S12); S10 before and after the whole package; S1, S2, S4, S8, S11 at the head.
