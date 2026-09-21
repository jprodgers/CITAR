#!/usr/bin/env bash
#
# CITAR installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash
#
# What it does, and nothing else:
#
#   * checks for a Python new enough to run CITAR, and says how to get one if there is not;
#   * installs CITAR into its own virtual environment, so nothing is added to the system Python;
#   * puts `citar` on the PATH;
#   * hands over to `citar setup`, which asks the questions.
#
# It never writes outside the install prefix and the shell profile line, never installs system
# packages, and never needs root for a personal install. A server install (--server) does use sudo,
# for the service account and the systemd unit, and says so before it does.
#
# Flags:
#   --server            install for a public server (implies a system-wide prefix)
#   --worker            install and configure the worker agent only
#   --prefix DIR        where to install (default: ~/.local/share/citar-app, or /opt/citar)
#   --version X.Y.Z     a specific release instead of the newest
#   --from PATH         install from a local wheel or checkout instead of PyPI
#   --no-setup          install only; do not run the setup wizard
#   --uninstall         remove what this script installed
set -euo pipefail

CITAR_MIN_PYTHON="3.11"
MODE="local"
PREFIX=""
VERSION=""
SOURCE=""
RUN_SETUP=1
UNINSTALL=0

# ----------------------------------------------------------------------------- output
# Colour only when writing to a terminal: piped into a file or a log, escape codes are noise.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
	BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; OFF=$'\033[0m'
else
	BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; OFF=""
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$BOLD" "$OFF" "$*"; }
warn() { printf '%s warn %s %s\n' "$YELLOW" "$OFF" "$*" >&2; }
die()  { printf '%s error%s %s\n' "$RED" "$OFF" "$*" >&2; exit 1; }
ok()   { printf '%s  ok  %s %s\n' "$GREEN" "$OFF" "$*"; }

usage() {
	sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; s/^set -euo.*//'
	exit 0
}

# ----------------------------------------------------------------------------- arguments
while [ $# -gt 0 ]; do
	case "$1" in
		--server)    MODE="server" ;;
		--worker)    MODE="worker" ;;
		--local)     MODE="local" ;;
		--prefix)    PREFIX="${2:?--prefix needs a directory}"; shift ;;
		--version)   VERSION="${2:?--version needs a version}"; shift ;;
		--from)      SOURCE="${2:?--from needs a path}"; shift ;;
		--no-setup)  RUN_SETUP=0 ;;
		--uninstall) UNINSTALL=1 ;;
		-h|--help)   usage ;;
		*)           die "Unknown option: $1 (try --help)" ;;
	esac
	shift
done

# ----------------------------------------------------------------------------- platform
case "$(uname -s)" in
	Linux)  PLATFORM="linux" ;;
	Darwin) PLATFORM="macos" ;;
	*)      die "This installer is for Linux and macOS. On Windows, download the installer from
       https://github.com/jprodgers/CITAR/releases, or run:  pip install citar" ;;
esac

if [ -z "$PREFIX" ]; then
	if [ "$MODE" = "server" ]; then
		PREFIX="/opt/citar"
	else
		PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}/citar-app"
	fi
fi
BIN_DIR="${HOME}/.local/bin"
[ "$MODE" = "server" ] && BIN_DIR="/usr/local/bin"

# `sudo` only where it is genuinely needed, and only if we are not already root.
SUDO=""
if [ "$MODE" = "server" ] && [ "$(id -u)" -ne 0 ]; then
	command -v sudo >/dev/null 2>&1 || die "A server install needs root. Re-run as root, or install sudo."
	SUDO="sudo"
fi

# ----------------------------------------------------------------------------- uninstall
if [ "$UNINSTALL" -eq 1 ]; then
	step "Removing CITAR"
	if [ "$MODE" = "server" ] && command -v systemctl >/dev/null 2>&1; then
		$SUDO systemctl disable --now citar 2>/dev/null || true
		$SUDO rm -f /etc/systemd/system/citar.service
		$SUDO systemctl daemon-reload 2>/dev/null || true
	fi
	$SUDO rm -rf "$PREFIX"
	for link in citar citar-admin citar-worker citar-mcp; do
		$SUDO rm -f "$BIN_DIR/$link"
	done
	ok "Removed the program from $PREFIX."
	say ""
	say "Your games, settings and results were NOT deleted. They are in:"
	say "  ${XDG_DATA_HOME:-$HOME/.local/share}/citar   (or /var/lib/citar for a server)"
	say "Delete that directory too if you want CITAR gone completely."
	exit 0
fi

# ----------------------------------------------------------------------------- python
# Find an interpreter that is new enough. Several are tried because the newest Python on a machine
# is frequently not the one called `python3`.
find_python() {
	local candidate
	for candidate in python3.14 python3.13 python3.12 python3.11 python3 python; do
		command -v "$candidate" >/dev/null 2>&1 || continue
		if "$candidate" -c "import sys; raise SystemExit(0 if sys.version_info[:2] >= tuple(int(p) for p in '${CITAR_MIN_PYTHON}'.split('.')) else 1)" 2>/dev/null; then
			printf '%s' "$candidate"
			return 0
		fi
	done
	return 1
}

step "Looking for Python ${CITAR_MIN_PYTHON} or newer"
if ! PYTHON="$(find_python)"; then
	say ""
	die "No Python ${CITAR_MIN_PYTHON}+ found.

  Debian / Ubuntu   sudo apt install python3 python3-venv
  Fedora / RHEL     sudo dnf install python3
  Arch              sudo pacman -S python
  macOS             brew install python@3.12   (or install from python.org)

Then run this installer again."
fi
ok "$($PYTHON -V) at $(command -v "$PYTHON")"

# python3-venv is a separate package on Debian and Ubuntu, and its absence is the single most
# common failure of an installer like this one. Checked here so the message names the package.
if ! "$PYTHON" -c "import venv, ensurepip" >/dev/null 2>&1; then
	die "This Python cannot create virtual environments.
On Debian or Ubuntu:  sudo apt install python3-venv
Then run this installer again."
fi

# ----------------------------------------------------------------------------- install
step "Installing CITAR into $PREFIX"
$SUDO mkdir -p "$PREFIX"

$SUDO "$PYTHON" -m venv "$PREFIX/venv"
VENV_PIP="$PREFIX/venv/bin/pip"
$SUDO "$VENV_PIP" install --quiet --upgrade pip wheel

# Extras by role: a home machine wants every provider client, a server does not need the local
# model libraries, and a worker needs only what it takes to serve one.
case "$MODE" in
	server) EXTRAS="server" ;;
	worker) EXTRAS="worker,openai" ;;
	*)      EXTRAS="all" ;;
esac
if [ -n "$SOURCE" ]; then
	# A local wheel or checkout. This is how CI tests the installer without a published release,
	# and how an offline install works.
	[ -e "$SOURCE" ] || die "No such file or directory: $SOURCE"
	SPEC="$SOURCE[$EXTRAS]"
else
	SPEC="citar[$EXTRAS]"
	[ -n "$VERSION" ] && SPEC="citar[$EXTRAS]==$VERSION"
fi

step "Downloading CITAR and its dependencies"
$SUDO "$VENV_PIP" install --quiet "$SPEC" || die "pip could not install $SPEC"
INSTALLED="$("$PREFIX/venv/bin/citar" --version 2>/dev/null || echo "CITAR")"
ok "$INSTALLED installed"

# ----------------------------------------------------------------------------- PATH
step "Putting citar on your PATH"
$SUDO mkdir -p "$BIN_DIR"
for command_name in citar citar-admin citar-worker citar-mcp; do
	[ -x "$PREFIX/venv/bin/$command_name" ] || continue
	$SUDO ln -sf "$PREFIX/venv/bin/$command_name" "$BIN_DIR/$command_name"
done
ok "Linked into $BIN_DIR"

if ! printf '%s' ":$PATH:" | grep -q ":$BIN_DIR:"; then
	# Appended rather than written: a login shell that does not read this file is common enough
	# (macOS Terminal reads .zprofile, not .zshrc, for login shells) that the line is added to the
	# one the user's shell is most likely to read, and then reported.
	PROFILE=""
	case "${SHELL##*/}" in
		zsh)  PROFILE="$HOME/.zshrc" ;;
		bash) [ "$PLATFORM" = "macos" ] && PROFILE="$HOME/.bash_profile" || PROFILE="$HOME/.bashrc" ;;
		fish) PROFILE="$HOME/.config/fish/config.fish" ;;
	esac
	if [ -n "$PROFILE" ] && [ "$MODE" != "server" ]; then
		mkdir -p "$(dirname "$PROFILE")"
		if ! grep -qs "citar installer" "$PROFILE" 2>/dev/null; then
			{
				printf '\n# Added by the citar installer\n'
				if [ "${SHELL##*/}" = "fish" ]; then
					printf 'fish_add_path %s\n' "$BIN_DIR"
				else
					printf 'export PATH="%s:$PATH"\n' "$BIN_DIR"
				fi
			} >> "$PROFILE"
			warn "$BIN_DIR was not on your PATH. Added it to $PROFILE."
			warn "Open a new terminal, or run:  export PATH=\"$BIN_DIR:\$PATH\""
		fi
	else
		warn "$BIN_DIR is not on your PATH. Add it, or run CITAR as $PREFIX/venv/bin/citar"
	fi
fi

# ----------------------------------------------------------------------------- setup
say ""
if [ "$RUN_SETUP" -eq 0 ]; then
	ok "Installed. Run 'citar setup' when you are ready."
	exit 0
fi

case "$MODE" in
	server) SETUP_FLAG="--server" ;;
	worker) SETUP_FLAG="--worker" ;;
	*)      SETUP_FLAG="--local" ;;
esac

# `curl … | bash` leaves standard input attached to the pipe the script itself came down, which is
# already at end of file. An interactive wizard reading that gets EOF on its first question and
# exits looking cancelled. Re-attaching to the terminal is what makes the one-line install work;
# where there is no terminal at all — CI, a Dockerfile, a provisioning script — the wizard is not
# run and the command to run later is printed instead.
if [ -t 0 ]; then
	step "Setting up"
	exec "$PREFIX/venv/bin/citar" setup "$SETUP_FLAG"
elif [ -r /dev/tty ]; then
	step "Setting up"
	exec "$PREFIX/venv/bin/citar" setup "$SETUP_FLAG" < /dev/tty
else
	ok "Installed."
	say ""
	say "There is no terminal here, so setup was not run. When you are ready:"
	say "    citar setup $SETUP_FLAG"
	say "or, to configure it without any questions:"
	say "    citar setup $SETUP_FLAG --non-interactive"
fi
