# Installing CITAR

Pick the row that matches you. They all end up in the same place.

| You want | Do this |
|---|---|
| To play, on Windows, without thinking about it | [The installer](#windows-installer) |
| To play, on macOS or Linux | [One line](#one-line-macos-and-linux) |
| A command you can upgrade and script | [pipx](#pipx-or-pip) (or [one line on Windows](#one-line-windows)) |
| A server other people sign in to | [Server install](#a-public-server) |
| To lend your GPU to somebody else's CITAR | [The CITAR helper](#the-citar-helper) |
| To change the code | [From source](#from-source) |

About 400 MB of disk for the program; saved games are a few hundred kilobytes each.

## Where CITAR is built for

Every release carries CITAR compiled for these platforms (its game engine is Rust, inside the package), and every
route on this page installs that build. Nothing is compiled on your machine.

| Platform | Needs |
|---|---|
| Windows x64 | Windows 10 or newer. The installer needs nothing else; the other routes need an x64 Python 3.11 or newer |
| macOS on Apple silicon or Intel | macOS 11 or newer, Python 3.11 or newer |
| Linux on x64 or ARM64 | glibc 2.28 or newer (Debian 10+, Ubuntu 20.04+, RHEL and its rebuilds 8+, Fedora, Amazon Linux 2023), Python 3.11 or newer |

Python means CPython, 3.11 or any newer release, in its regular build: the free-threaded builds (`python3.13t`,
`python3.14t`) cannot load CITAR's engine, so use the regular `python3.14` beside them. On Windows on ARM, install
the x64 Python (Windows runs it) or use the installer.

**Anywhere else** (musl distributions such as Alpine, 32-bit systems, an older glibc, another CPU) there is no prebuilt
CITAR, and the install scripts say so before they change anything. `pip install citar` then builds it from the source
release, which needs a Rust toolchain: see [From source](#from-source). The [CITAR helper](#the-citar-helper) is built
for the same five platforms.

---

## Windows installer

Download `CITAR-<version>-setup.exe` from the
[releases page](https://github.com/jprodgers/CITAR/releases/latest) and run it.

It installs per-user, so there is no administrator prompt, and it bundles everything (you do not need Python). You
get a Start Menu folder with the game, `CITAR setup`, "Check my CITAR installation" (`citar doctor`) and your saved
games' folder. A desktop shortcut and the `citar` command on your PATH are boxes to tick on the way (the PATH one
starts unticked). A newer installer upgrades in place: the same folder, the same saved games.

The same build comes as a portable zip, `CITAR-<version>-windows-x64.zip`: unzip it anywhere and run `citar-play.exe`
(the game, no console window) or `citar.exe` (the command line). Scoop installs that zip (below).

**"Windows protected your PC".** The installer is not code-signed, so SmartScreen warns about it
until enough people have run it. Click **More info → Run anyway**, after
[checking the file](#checking-a-download) if you like. Why it is not signed is explained in
[packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md#code-signing).

**To remove it:** Settings → Apps → CITAR, or the uninstaller in the Start Menu folder. It asks before deleting your
saved games, and the answer it defaults to is no (a silent uninstall, `/VERYSILENT /SUPPRESSMSGBOXES`, keeps them).

---

## One line (macOS and Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash
```

It checks that CITAR is built for this machine and finds a Python that can run it (the newest 3.11 or later), installs
CITAR's wheel into a virtual environment of its own under `~/.local/share/citar-app`, links `citar`, `citar-admin` and
`citar-mcp` into `~/.local/bin` (adding that folder to your shell profile's PATH if it is not there), and hands over to
the setup wizard. Nothing is installed system-wide and it does not need root.

Read it first if you would rather:

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | less
```

Its options, after `bash -s --` in the one line (or on `bash install.sh` with the file downloaded):

| Option | Does |
|---|---|
| `--version X.Y.Z` | a given release rather than the newest |
| `--python PATH` | the Python to install with |
| `--prefix DIR`, `--bin-dir DIR` | where the program and the commands go |
| `--no-profile` | never add a line to a shell profile |
| `--from FILE` or `--from DIR` | a wheel you already have, or a folder of wheels (it picks this machine's), instead of PyPI |
| `--no-setup` | install only; do not run the wizard |
| `--plan` | print every step it would take, with every path, and change nothing |
| `--worker` | [the CITAR helper](#the-citar-helper) instead of the game |
| `--uninstall` | remove what it installed (below) |

`--prefix` must be a new or empty folder, or an earlier install of CITAR's: the script marks its own folder and never
installs over anything else.

**On a machine CITAR is not built for** the script stops before it changes anything and says what it found, for
example: "There is no prebuilt CITAR for this platform: Linux with musl libc, such as Alpine (the builds need glibc
2.28 or newer). 'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs)." It never builds
CITAR itself. A free-threaded Python is named too, with the regular build to use instead.

**To remove it:** `bash install.sh --uninstall` (with the same `--prefix` and `--bin-dir` if you gave them). It removes
the program folder, if it is CITAR's, and the commands in the bin folder that point into it, and nothing else. Your
games and settings are left alone.

---

## One line (Windows)

For a command you can run, upgrade and script, from PowerShell:

```powershell
& ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/install.ps1)))
```

It checks for an x64 Python 3.11 or newer (the `py` launcher's, newest first), installs CITAR's Windows wheel into
`%LOCALAPPDATA%\CITAR\app`, puts `citar.cmd` and the other commands in `%LOCALAPPDATA%\CITAR\bin`, adds that folder to
your user PATH, and runs the setup wizard. No administrator, nothing system-wide. Run as a script block like this, it
takes parameters after the last `)` and leaves your PowerShell window open when it stops (`$LASTEXITCODE` says how it
ended).

| Parameter | Does |
|---|---|
| `-Version X.Y.Z` | a given release |
| `-Python PATH` | the Python to install with |
| `-Prefix DIR`, `-BinDir DIR` | where the program and the commands go |
| `-NoPath` | leave your PATH alone: nothing is written to the registry |
| `-From FILE` or `-From DIR` | a wheel, or a folder of wheels, instead of PyPI |
| `-NoSetup` | install only |
| `-Worker` | [the CITAR helper](#the-citar-helper) instead of the game |
| `-Uninstall` | remove what it installed (with `-Prefix`/`-BinDir` if you gave them) |

Your PATH is changed by appending the bin folder to it as it is stored (its `%VARIABLES%` kept), and the uninstall
takes away exactly that entry, so the two leave it byte for byte as it was.

---

## pipx or pip

[pipx](https://pipx.pypa.io) is the better choice for an application: it keeps CITAR's dependencies
in their own environment while putting the command on your PATH.

```bash
pipx install "citar[all]"
citar setup
```

With plain pip, use a virtual environment:

```bash
python -m venv ~/citar && source ~/citar/bin/activate
pip install "citar[all]"
```

On the [platforms above](#where-citar-is-built-for) pip takes CITAR's wheel. Anywhere else it falls back to the
source release and needs a Rust toolchain; to make sure it never does, say so: `pip install --only-binary=citar
"citar[all]"` fails at once instead.

### Extras

`citar` on its own installs the game, the web client and the server. The extras add provider
clients you may not need:

| Extra | Adds |
|---|---|
| `anthropic` | Claude through the Anthropic API |
| `openai` | OpenAI-compatible endpoints — LM Studio, Ollama, vLLM, llama.cpp |
| `mcp` | MCP clients such as Claude Code taking a seat |
| `oauth` | Google, GitHub, Discord and Microsoft sign-in |
| `keyring` | API keys in the OS credential store |
| `postgres` | PostgreSQL instead of SQLite |
| `all` | Everything above except `postgres` |
| `server` | What a public deployment needs, without the local-model clients |
| `dev` | `all`, plus the test, lint and documentation tools |

```bash
pipx install "citar[anthropic,mcp]"      # Claude only, no local models
```

### Upgrading

```bash
pipx upgrade citar
```

Migrations run at startup, so upgrading is: install, restart. Saves from an older CITAR load as
long as the ruleset version has not changed; when it has, the release notes say so. The install scripts upgrade by
running them again.

---

## Homebrew, Scoop, winget

| Manager | The game | The CITAR helper |
|---|---|---|
| Homebrew (macOS, Linux) | not through Homebrew: use [one line](#one-line-macos-and-linux) or [pipx](#pipx-or-pip) | `brew install jprodgers/citar/citar-helper` |
| Scoop (Windows) | `scoop install citar` (the portable zip) | `scoop install citar-helper` |
| winget (Windows) | `winget install JimmieRodgers.CITAR` (the installer), once accepted | `winget install JimmieRodgers.CITAR.Helper`, once accepted |

```bash
brew install jprodgers/citar/citar-helper
```

```powershell
scoop bucket add citar https://github.com/jprodgers/scoop-citar
scoop install citar citar-helper
```

The Homebrew tap holds only the helper. 0.1.5's `citar` formula built the whole game from source, which now needs a
Rust toolchain it never asked for; if you have it, `brew uninstall citar`, then install the game with `install.sh` or
pipx.

winget is not available yet: a first submission to `microsoft/winget-pkgs` is reviewed by a person, and CITAR's have
not been accepted. Until they are, use Scoop or the [installer](https://github.com/jprodgers/CITAR/releases/latest).

A package manager's helper is its **packaged** build, which never updates itself: `brew upgrade citar-helper`,
`scoop update citar-helper` and `winget upgrade` do (see [the helper's updates](server/WORKERS.md#updates)). All three
lag a release by the day or so it takes to publish their manifests after it. See
[packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md).

---

## Docker

For a server. For playing on your own machine it adds nothing.

```bash
docker run -d -p 127.0.0.1:8765:8765 -v citar:/var/lib/citar ghcr.io/jprodgers/citar:latest
```

That is local mode, the single-owner mode a laptop runs in, so publish it on `127.0.0.1` only: anyone who reaches the
port is its owner. For a real deployment with accounts and TLS, use
[docker-compose.yml](https://github.com/jprodgers/CITAR/blob/main/docker-compose.yml), or publish the container on
`127.0.0.1` behind a proxy of your own; see [server/DEPLOY.md](server/DEPLOY.md#docker).

The image is built for `linux/amd64` and `linux/arm64` (Docker picks yours), from the release's own wheels on
Debian 13 with Python 3.12, about 280 MB. It runs as uid and gid 10001, keeps everything in the one volume at
`/var/lib/citar`, and checks its own health by asking the server. Tags: `:<version>` (a release),
`:<major>.<minor>` (the newest release of that series) and `:latest`.

---

## A public server

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --server
```

This one does use `sudo`, and says so before it does: it creates a service account, installs the release into
`/opt/citar/releases/<version>` with `/opt/citar/current` pointing at it, and then runs `citar setup --server`, which
asks for your domain and writes the systemd unit, the nginx site and the TLS certificate. `--plan` prints every step
first and changes nothing.

Full walkthrough, including what to do when you have an nginx already, upgrades, and the scripted deploy a server
with other sites on it should use: [server/DEPLOY.md](server/DEPLOY.md).

---

## The CITAR helper

The CITAR helper lends this machine's models (and, if you say so, its spare CPU) to a CITAR server
somewhere else. It is one file with nothing else to install, and its connection is outbound, so
nothing needs to be opened on your router. There are five builds: Windows x64, macOS on Apple silicon
and on Intel, Linux x64 and ARM64 (glibc 2.28 or newer, as the game). Since 0.1.6 the helper replaces the Python
worker: `citar worker` says where to get it. Run it with a token from whoever runs the server; they issue one on the
Servers page, or with `citar admin add-server --name "your machine"`.

| Route | Gives you |
|---|---|
| The server's **Servers** page, or the [latest release](https://github.com/jprodgers/CITAR/releases/latest) | the self-updating build for your computer (a Mac's Safari or Firefox gets both macOS files, since neither says which processor the Mac has) |
| `curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh \| bash -s -- --worker` | the same, checked against its `.sha256` before anything is written, as `citar-helper` in `~/.local/bin` |
| `& ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/install.ps1))) -Worker` | the same on Windows, as `citar-helper.exe` in `%LOCALAPPDATA%\CITAR\bin` |
| Homebrew, Scoop or winget ([above](#homebrew-scoop-winget)) | the packaged build, which its package manager updates |

The self-updating build installs a new release its server offers once its signature checks out; the packaged build
leaves that to its package manager and refuses `"updates": "auto"`. Then `citar-helper setup`, and
`citar-helper service install` keeps it running with the machine or from your sign-in (with Homebrew,
`brew services start citar-helper`). A downloaded macOS file needs `xattr -d com.apple.quarantine citar-helper-macos-*`
first (it is not notarised); one installed by `install.sh` or Homebrew does not. See
[server/WORKERS.md](server/WORKERS.md).

---

## From source

```bash
git clone https://github.com/jprodgers/CITAR
cd CITAR
pip install -e ".[dev]"
python -m unittest discover -s tests
citar serve --debug
```

**Building from source needs a Rust toolchain.** CITAR's game engine is written in Rust and ships
inside the package as a compiled extension, so `pip install` from a checkout (or from the source
release, `citar-<version>.tar.gz`, which is what pip takes where no wheel fits) compiles it. Install
[rustup](https://rustup.rs) first; on Windows, let its installer add the Visual Studio C++ build tools. The
repository's `rust-toolchain.toml` names the exact Rust release, and rustup downloads it the first time. The first
build takes a few minutes; later ones only rebuild what changed. A build of your own reports its label as `unknown`
in `citar doctor`, where a release's says `v<version>`. The installer, the install scripts, pipx and the release wheels
need none of this.

An editable install builds the engine without its test operations, so about 200 of the tests (the
rule scripts among them) skip; the dev loop of
[CONTRIBUTING.md](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md#rust),
`cargo xtask develop`, builds it with them and runs every test.

A checkout keeps its state beside the code — `saves/`, `config/`, `benchmarks/` — rather than in
your user directory, so a contributor's test games are visible, diffable and easy to delete.
[CONTRIBUTING.md](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md) has the rest.

---

## Checking a download

Every file of a release is listed with its SHA-256 in the release's `SHA256SUMS`, and each has a `.sha256` beside
it:

```bash
sha256sum -c SHA256SUMS --ignore-missing        # in the folder you downloaded into
```

```powershell
Get-FileHash .\CITAR-<version>-setup.exe -Algorithm SHA256    # compare with CITAR-<version>-setup.exe.sha256
```

Every file is also built in a [public CI run](https://github.com/jprodgers/CITAR/actions) whose log you can read, and
carries a build provenance attestation that says so. With the [GitHub CLI](https://cli.github.com), check that a file
was made by CITAR's release workflow, on GitHub's own runners, for that release's tag (`<version>` is the
release's, such as `0.1.6`):

```bash
gh attestation verify CITAR-<version>-setup.exe --repo jprodgers/CITAR \
  --signer-workflow jprodgers/CITAR/.github/workflows/release.yml \
  --source-ref refs/tags/v<version> --deny-self-hosted-runners
gh attestation verify oci://ghcr.io/jprodgers/citar:<version> --repo jprodgers/CITAR \
  --signer-workflow jprodgers/CITAR/.github/workflows/release.yml \
  --source-ref refs/tags/v<version> --deny-self-hosted-runners
```

PyPI keeps its own attestations of the wheels it serves, made by the same workflow.

---

## Checking an installation

```bash
citar doctor
```

Versions, the build and its label, directories, dependencies, database, ruleset, a job host started for a moment
(its jobs line says whether this machine's results can be checked against the committed ones), every model endpoint
it can reach, and whether the port is free. It changes nothing, and its output is what a bug report should contain.

```bash
citar where
```

Just the directories.

---

## Uninstalling

| Installed with | Remove with |
|---|---|
| The Windows installer | Settings → Apps → CITAR |
| The portable zip | delete its folder |
| `install.sh` | `bash install.sh --uninstall` (`--worker` too, for a helper installed with it) |
| `install.ps1` | `& ([scriptblock]::Create((iwr -useb https://raw.githubusercontent.com/jprodgers/CITAR/main/install.ps1))) -Uninstall` |
| pipx | `pipx uninstall citar` |
| pip | `pip uninstall citar` |
| Homebrew | `brew uninstall citar-helper` (and `brew uninstall citar`, 0.1.5's formula, if you still have it) |
| Scoop | `scoop uninstall citar citar-helper` |
| winget | `winget uninstall JimmieRodgers.CITAR` (and `JimmieRodgers.CITAR.Helper`) |
| Docker | `docker rm -f <container> && docker volume rm citar` |

A helper running as a service is stopped first: `citar-helper service uninstall` (`brew services stop citar-helper`
with Homebrew). The install scripts refuse to remove a helper a service still runs.

None of these delete your games, settings or results. Those live in the directory `citar where`
prints, and you have to delete it yourself:

| Platform | Directory |
|---|---|
| Windows | `%LOCALAPPDATA%\CITAR` |
| macOS | `~/Library/Application Support/CITAR` |
| Linux | `~/.local/share/citar` |
| Server install | `/var/lib/citar` |
