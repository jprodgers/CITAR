# Installing CITAR

Pick the row that matches you. They all end up in the same place.

| You want | Do this |
|---|---|
| To play, on Windows, without thinking about it | [The installer](#windows-installer) |
| To play, on macOS or Linux | [One line](#one-line-macos-and-linux) |
| A command you can upgrade and script | [pipx](#pipx-or-pip) |
| A server other people sign in to | [Server install](#a-public-server) |
| To lend your GPU to somebody else's CITAR | [Worker](#a-worker) |
| To change the code | [From source](#from-source) |

Python 3.11 or newer, on Windows 10+, macOS 12+, or any current Linux. About 400 MB of disk for the
program; saved games are a few hundred kilobytes each.

---

## Windows installer

Download `CITAR-<version>-setup.exe` from the
[releases page](https://github.com/jprodgers/CITAR/releases/latest) and run it.

It installs per-user, so there is no administrator prompt, and it bundles everything — you do not
need Python. You get a Start Menu entry for the game, one for `citar setup`, and one for
`citar doctor`.

**"Windows protected your PC".** The installer is not code-signed, so SmartScreen warns about it
until enough people have run it. Click **More info → Run anyway**, or check the file against the
SHA-256 published on the release page first:

```powershell
Get-FileHash .\CITAR-0.1.4-setup.exe -Algorithm SHA256
```

Every artifact is built in a [public CI run](https://github.com/jprodgers/CITAR/actions) whose log
you can read. Why it is not signed is explained in
[packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md#code-signing).

**To remove it:** Settings → Apps, or the uninstaller in the Start Menu folder. It asks before
deleting your saved games.

---

## One line (macOS and Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash
```

It checks for a suitable Python, installs CITAR into its own virtual environment under
`~/.local/share/citar-app`, links `citar` into `~/.local/bin`, and hands over to the setup wizard.
Nothing is installed system-wide and it does not need root.

Read it first if you would rather:

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | less
```

To remove it: `bash install.sh --uninstall`. Your games and settings are left alone.

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
| `worker` | The worker agent |
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
long as the ruleset version has not changed; when it has, the release notes say so.

---

## Homebrew, Scoop, winget

```bash
brew install jprodgers/citar/citar             # macOS and Linux
```

```powershell
scoop bucket add citar https://github.com/jprodgers/scoop-citar
scoop install citar
```

winget is not available yet: the manifest is written, but a first submission to
`microsoft/winget-pkgs` is reviewed by a person and has not been accepted yet. Until it is, use
Scoop or the [installer](https://github.com/jprodgers/CITAR/releases/latest) on Windows.

All three lag PyPI, because each needs the release published before it can point at anything. See
[packaging/README.md](https://github.com/jprodgers/CITAR/blob/main/packaging/README.md).

---

## Docker

For a server. For playing on your own machine it adds nothing.

```bash
docker run -d -p 8765:8765 -v citar:/var/lib/citar ghcr.io/jprodgers/citar:latest
```

That is local mode on loopback inside the container. For a real deployment with TLS, use
[docker-compose.yml](https://github.com/jprodgers/CITAR/blob/main/docker-compose.yml) — see [server/DEPLOY.md](server/DEPLOY.md).

The image is about 250 MB, runs as a non-root user, and keeps everything in the one volume at
`/var/lib/citar`.

---

## A public server

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --server
```

This one does use `sudo`, and says so before it does: it creates a service account, installs CITAR
to `/opt/citar`, and then runs `citar setup --server`, which asks for your domain and writes the
systemd unit, the nginx site and the TLS certificate.

Full walkthrough, including what to do when you have an nginx already: [server/DEPLOY.md](server/DEPLOY.md).

---

## A worker

A worker lends this machine's models to a CITAR server somewhere else. The connection is outbound,
so nothing needs to be opened on your router.

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --worker
```

You need a token from whoever runs the server; they get one with
`citar admin add-server --name "your machine"`. See [server/WORKERS.md](server/WORKERS.md).

---

## From source

```bash
git clone https://github.com/jprodgers/CITAR
cd CITAR
pip install -e ".[dev]"
python -m unittest discover -s tests
citar serve --debug
```

A checkout keeps its state beside the code — `saves/`, `config/`, `benchmarks/` — rather than in
your user directory, so a contributor's test games are visible, diffable and easy to delete.
[CONTRIBUTING.md](https://github.com/jprodgers/CITAR/blob/main/CONTRIBUTING.md) has the rest.

---

## Checking an installation

```bash
citar doctor
```

Versions, directories, dependencies, database, ruleset, every model endpoint it can reach, and
whether the port is free. It changes nothing, and its output is what a bug report should contain.

```bash
citar where
```

Just the directories.

---

## Uninstalling

| Installed with | Remove with |
|---|---|
| The Windows installer | Settings → Apps → CITAR |
| `install.sh` | `bash install.sh --uninstall` |
| pipx | `pipx uninstall citar` |
| pip | `pip uninstall citar` |
| Homebrew | `brew uninstall citar` |
| Scoop | `scoop uninstall citar` |
| Docker | `docker rm -f <container> && docker volume rm citar` |

None of these delete your games, settings or results. Those live in the directory `citar where`
prints, and you have to delete it yourself:

| Platform | Directory |
|---|---|
| Windows | `%LOCALAPPDATA%\CITAR` |
| macOS | `~/Library/Application Support/CITAR` |
| Linux | `~/.local/share/citar` |
| Server install | `/var/lib/citar` |
