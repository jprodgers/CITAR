#!/usr/bin/env bash
#
# CITAR installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --worker
#
# What it does, and nothing else:
#
#   * checks that CITAR is built for this machine (Linux on x86_64 or aarch64 with glibc 2.28 or newer, or macOS 11
#     or newer) and that a Python new enough to run it is here, and says plainly what to do when either is not;
#   * installs CITAR into its own virtual environment from a prebuilt wheel: it never builds CITAR from source, so it
#     needs no compiler and no Rust toolchain, and nothing is added to the system Python;
#   * puts `citar` on the PATH;
#   * hands over to `citar setup`, which asks the questions.
#
# With --worker it installs the CITAR helper instead, the one program a machine needs to lend its models (and, if
# you choose, its spare CPU) to a CITAR server: no Python, no game. It downloads the helper built for this machine
# with its .sha256, checks it before anything is written, puts citar-helper in the bin folder and says what to run
# next. This helper updates itself when its server offers a signed update.
#
# It never writes outside the install prefix, the bin folder and the shell profile line, never installs system
# packages, and never needs root for a personal install. A server install (--server) does use sudo, for the service
# account and the systemd unit, and says so before it does.
#
# Flags:
#   --server            install for a public server (implies a system-wide prefix)
#   --worker            install the CITAR helper (citar-helper) instead of the game
#   --prefix DIR        where to install the game (default: ~/.local/share/citar-app, or /opt/citar)
#   --bin-dir DIR       where the commands go (default: ~/.local/bin, or /usr/local/bin)
#   --no-profile        never add the bin folder to a shell profile
#   --python PATH       the Python to install the game with (default: the newest 3.11 or later found)
#   --version X.Y.Z     a specific release instead of the newest
#   --from PATH         install the game from a local wheel, or a folder of wheels, instead of PyPI
#   --no-setup          install only; do not run the setup wizard
#   --uninstall         remove what this script installed (with --worker: the helper only)
#
# Environment:
#   CITAR_RELEASE_URL   where --worker downloads from (default https://github.com/jprodgers/CITAR/releases):
#                       an https address, or http to this machine only
set -euo pipefail

CITAR_MIN_PYTHON="3.11"
RELEASES_DEFAULT="https://github.com/jprodgers/CITAR/releases"
MODE="local"
WORKER=0
PREFIX=""
BIN_DIR=""
NO_PROFILE=0
PYTHON=""
VERSION=""
SOURCE=""
RUN_SETUP=1
UNINSTALL=0

# ----------------------------------------------------------------------------- output
# Colour only when writing to a terminal: piped into a file or a log, escape codes are noise.
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
	BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; OFF=$'\033[0m'
else
	BOLD=""; RED=""; GREEN=""; YELLOW=""; OFF=""
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$BOLD" "$OFF" "$*"; }
warn() { printf '%s warn %s %s\n' "$YELLOW" "$OFF" "$*" >&2; }
die()  { printf '%s error%s %s\n' "$RED" "$OFF" "$*" >&2; exit 1; }
ok()   { printf '%s  ok  %s %s\n' "$GREEN" "$OFF" "$*"; }

usage() {
	if [ -f "$0" ]; then
		sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//; s/^set -euo.*//'
	else
		say "Usage: install.sh [--server | --worker] [--prefix DIR] [--bin-dir DIR] [--no-profile] [--python PATH]"
		say "                  [--version X.Y.Z] [--from PATH] [--no-setup] [--uninstall]"
		say "Every flag is described at the top of https://github.com/jprodgers/CITAR/blob/main/install.sh"
	fi
	exit 0
}

# ----------------------------------------------------------------------------- arguments
while [ $# -gt 0 ]; do
	case "$1" in
		--server)     MODE="server" ;;
		--worker)     WORKER=1 ;;
		--local)      MODE="local" ;;
		--prefix)     PREFIX="${2:?--prefix needs a directory}"; shift ;;
		--bin-dir)    BIN_DIR="${2:?--bin-dir needs a directory}"; shift ;;
		--no-profile) NO_PROFILE=1 ;;
		--python)     PYTHON="${2:?--python needs an interpreter}"; shift ;;
		--version)    VERSION="${2:?--version needs a version}"; shift ;;
		--from)       SOURCE="${2:?--from needs a path}"; shift ;;
		--no-setup)   RUN_SETUP=0 ;;
		--uninstall)  UNINSTALL=1 ;;
		-h|--help)    usage ;;
		*)            die "Unknown option: $1 (try --help)" ;;
	esac
	shift
done

VERSION="${VERSION#v}"
if [ -n "$VERSION" ]; then
	semver='^[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta|rc)\.[0-9]+)?$'
	[[ "$VERSION" =~ $semver ]] || die "--version $VERSION is not a CITAR release (X.Y.Z, or X.Y.Z-rc.N)"
fi
if [ "$WORKER" -eq 1 ]; then
	[ "$MODE" = "server" ] && die "--worker installs the helper and --server a server: choose one"
	[ -n "$SOURCE" ] && die "--from installs the game from a wheel; --worker downloads the helper: choose one"
fi

# A folder given relative to here, made absolute (and without a trailing slash): the links point into the prefix,
# and the uninstall compares them with it.
absolute() {
	local p="$1"
	case "$p" in /*) ;; *) p="$PWD/$p" ;; esac
	while [ "${#p}" -gt 1 ] && [ "${p%/}" != "$p" ]; do p="${p%/}"; done
	printf '%s' "$p"
}
on_path() { case ":$PATH:" in *":$1:"*) return 0 ;; esac; return 1; }

# Temporary folders, removed however the script ends (and before it hands over to the setup wizard).
HELPER_TMP=""
PICKED=""
cleanup() {
	[ -z "$HELPER_TMP" ] || rm -rf "$HELPER_TMP"
	[ -z "$PICKED" ] || rm -rf "$PICKED"
	HELPER_TMP=""; PICKED=""
}
trap cleanup EXIT

if [ -z "$PREFIX" ]; then
	if [ "$MODE" = "server" ]; then
		PREFIX="/opt/citar"
	else
		PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}/citar-app"
	fi
fi
if [ -z "$BIN_DIR" ]; then
	BIN_DIR="${HOME}/.local/bin"
	[ "$MODE" = "server" ] && BIN_DIR="/usr/local/bin"
fi
PREFIX="$(absolute "$PREFIX")"
BIN_DIR="$(absolute "$BIN_DIR")"
case "$PREFIX" in
	/|"$(absolute "$HOME")") die "--prefix $PREFIX would hold more than CITAR: name a folder of its own" ;;
esac

# `sudo` only where it is genuinely needed, and only if we are not already root.
SUDO=""
if [ "$MODE" = "server" ] && [ "$(id -u)" -ne 0 ]; then
	command -v sudo >/dev/null 2>&1 || die "A server install needs root. Re-run as root, or install sudo."
	SUDO="sudo"
fi

# ----------------------------------------------------------------------------- the platform
# What this machine is, as the checks below read it. CITAR_INSTALL_TEST_FACTS ("name=value ...", for the tests)
# replaces any of the probed facts: system, machine, libc (glibc-X.Y, musl or unknown) and macos, and the Python's:
# python (X.Y), implementation, free_threaded (0 or 1) and bits.
test_fact() {
	local pair found=1
	set -f
	for pair in ${CITAR_INSTALL_TEST_FACTS:-}; do
		case "$pair" in "$1="*) printf '%s' "${pair#*=}"; found=0; break ;; esac
	done
	set +f
	return "$found"
}

probe_libc() {
	local v
	if v="$(getconf GNU_LIBC_VERSION 2>/dev/null)"; then
		case "$v" in "glibc "*) printf 'glibc-%s' "${v#glibc }"; return 0 ;; esac
	fi
	v="$(ldd --version 2>&1 || true)"
	case "$v" in
		*[Mm]usl*) printf 'musl' ;;
		*) if ls /lib/ld-musl-* >/dev/null 2>&1; then printf 'musl'; else printf 'unknown'; fi ;;
	esac
}

# Whether the dotted version $1 is $2 or newer (numbers only: "2.28", "11", "15.1"); anything else is not.
at_least() {
	local IFS=. i x y
	local -a a b
	set -f
	a=($1); b=($2)
	set +f
	[ "${#a[@]}" -gt 0 ] || return 1
	for i in 0 1 2; do
		x="${a[i]:-0}"; y="${b[i]:-0}"
		case "$x$y" in *[!0-9]*) return 1 ;; esac
		[ "$((10#$x))" -gt "$((10#$y))" ] && return 0
		[ "$((10#$x))" -lt "$((10#$y))" ] && return 1
	done
	return 0
}

# One message for every platform CITAR is not built for (design/phase5-release.md P5.8), then exit.
no_prebuilt() {
	if [ "$WORKER" -eq 1 ]; then
		die "There is no prebuilt CITAR helper for this platform: $1.
The helper builds from CITAR's source with a Rust toolchain (https://rustup.rs): in a clone of
https://github.com/jprodgers/CITAR, run  cargo build --release -p citar-helper"
	fi
	die "There is no prebuilt CITAR for this platform: $1.
'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs)."
}

# The platforms CITAR's wheels and helpers are built for: one table for both (P5.3.2, P5.4.1). Sets PLATFORM and
# HELPER_ASSET, or explains and exits.
check_platform() {
	local system machine libc macos arch
	system="$(test_fact system || uname -s)"
	machine="$(test_fact machine || uname -m)"
	case "$system" in
		Linux)
			case "$machine" in
				x86_64|amd64)  arch="x64" ;;
				aarch64|arm64) arch="arm64" ;;
				*) no_prebuilt "Linux on $machine (the builds are for x86_64 and aarch64)" ;;
			esac
			libc="$(test_fact libc || probe_libc)"
			case "$libc" in
				glibc-*)
					at_least "${libc#glibc-}" 2.28 || no_prebuilt "glibc ${libc#glibc-} (the builds need glibc 2.28 or newer: \
Debian 10, Ubuntu 20.04, RHEL 8 and later have it)" ;;
				musl) no_prebuilt "Linux with musl libc, such as Alpine (the builds need glibc 2.28 or newer)" ;;
				*)    no_prebuilt "Linux without glibc, as far as getconf GNU_LIBC_VERSION says (the builds need glibc 2.28 or newer)" ;;
			esac
			PLATFORM="linux" ;;
		Darwin)
			case "$machine" in
				arm64|aarch64) arch="arm64" ;;
				x86_64)        arch="x64" ;;
				*) no_prebuilt "macOS on $machine (the builds are for Apple silicon and Intel)" ;;
			esac
			macos="$(test_fact macos || sw_vers -productVersion 2>/dev/null || printf 'unknown')"
			at_least "$macos" 11 || no_prebuilt "macOS $macos (the builds need macOS 11 or newer)"
			PLATFORM="macos" ;;
		*)
			die "This installer is for Linux and macOS. On Windows, download the installer from
       https://github.com/jprodgers/CITAR/releases, or run install.ps1 in PowerShell" ;;
	esac
	HELPER_ASSET="citar-helper-$PLATFORM-$arch"
}

# ----------------------------------------------------------------------------- PATH
# Puts BIN_DIR on the PATH of the user's shell (one line in its profile), unless --no-profile or a server install.
put_on_path() {
	local profile=""
	on_path "$BIN_DIR" && return 0
	if [ "$NO_PROFILE" -eq 1 ] || [ "$MODE" = "server" ]; then
		warn "$BIN_DIR is not on your PATH: run the commands from there, or add it to your PATH."
		return 0
	fi
	# Appended rather than written: a login shell that does not read this file is common enough (macOS Terminal
	# reads .zprofile, not .zshrc, for login shells) that the line is added to the one the user's shell is most
	# likely to read, and then reported.
	case "${SHELL##*/}" in
		zsh)  profile="$HOME/.zshrc" ;;
		bash) [ "$PLATFORM" = "macos" ] && profile="$HOME/.bash_profile" || profile="$HOME/.bashrc" ;;
		fish) profile="$HOME/.config/fish/config.fish" ;;
	esac
	if [ -z "$profile" ]; then
		warn "$BIN_DIR is not on your PATH. Add it to your shell's PATH to run the commands by name."
		return 0
	fi
	mkdir -p "$(dirname "$profile")"
	if ! grep -qsF "$BIN_DIR" "$profile"; then
		{
			printf '\n# Added by the citar installer\n'
			if [ "${SHELL##*/}" = "fish" ]; then
				printf 'fish_add_path %s\n' "$BIN_DIR"
			else
				printf 'export PATH="%s:$PATH"\n' "$BIN_DIR"
			fi
		} >> "$profile"
		warn "$BIN_DIR was not on your PATH. Added it to $profile."
		warn "Open a new terminal, or run:  export PATH=\"$BIN_DIR:\$PATH\""
	fi
}

# ----------------------------------------------------------------------------- uninstall
# The helper as a service names its binary in its unit or LaunchAgent: removing the file under it would leave a
# service that cannot start, so its own uninstall comes first.
helper_service() {
	local definition
	for definition in "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/citar-helper.service" \
		/etc/systemd/system/citar-helper.service "$HOME/Library/LaunchAgents/org.citar.helper.plist"; do
		if [ -f "$definition" ] && grep -qF "$BIN_DIR/citar-helper" "$definition" 2>/dev/null; then
			printf '%s' "$definition"
			return 0
		fi
	done
	return 1
}

# A prefix this script (or 0.1.5's) installed: its marker, or the game's venv. Nothing else is ever deleted.
is_citar_prefix() { [ -f "$1/.citar-installer" ] || [ -x "$1/venv/bin/citar" ]; }

if [ "$UNINSTALL" -eq 1 ]; then
	step "Removing CITAR"
	if [ -e "$BIN_DIR/citar-helper" ] && service="$(helper_service)"; then
		die "The helper at $BIN_DIR/citar-helper runs as a service ($service). Remove the service first:
    $BIN_DIR/citar-helper service uninstall      (add --system for the machine's)
then run this again. Nothing was removed."
	fi
	if [ "$WORKER" -eq 0 ]; then
		if [ "$MODE" = "server" ] && command -v systemctl >/dev/null 2>&1; then
			$SUDO systemctl disable --now citar 2>/dev/null || true
			$SUDO rm -f /etc/systemd/system/citar.service
			$SUDO systemctl daemon-reload 2>/dev/null || true
		fi
		if [ -d "$PREFIX" ] && [ -z "$(ls -A "$PREFIX" 2>/dev/null)" ]; then
			$SUDO rmdir "$PREFIX"
		elif [ -e "$PREFIX" ]; then
			is_citar_prefix "$PREFIX" || die "$PREFIX does not hold a CITAR install (no .citar-installer, no venv/bin/citar): nothing was removed."
			$SUDO rm -rf "$PREFIX"
			ok "Removed the program from $PREFIX."
		fi
		# Only the links that point into the prefix: a command of the same name from elsewhere is not ours.
		for link in citar citar-admin citar-worker citar-mcp; do
			if [ -L "$BIN_DIR/$link" ]; then
				case "$(readlink "$BIN_DIR/$link")" in "$PREFIX"/*) $SUDO rm -f "$BIN_DIR/$link" ;; esac
			fi
		done
	fi
	if [ -e "$BIN_DIR/citar-helper" ] || [ -e "$BIN_DIR/citar-helper.old" ]; then
		# the binary, the one its last update replaced, and an update's half-written file
		$SUDO rm -f "$BIN_DIR/citar-helper" "$BIN_DIR/citar-helper.old" "$BIN_DIR/.citar-helper.new"
		ok "Removed citar-helper from $BIN_DIR."
	fi
	say ""
	say "Your games, settings and results were NOT deleted. They are in:"
	say "  ${XDG_DATA_HOME:-$HOME/.local/share}/citar   (or /var/lib/citar for a server)"
	say "Delete that directory too if you want CITAR gone completely."
	exit 0
fi

check_platform

# ----------------------------------------------------------------------------- the helper
sha256_of() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d' ' -f1
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | cut -d' ' -f1
	else
		openssl dgst -sha256 -r "$1" | cut -d' ' -f1
	fi
}

# Whether a URL's host is this machine: localhost, [::1] or 127.a.b.c.
is_loopback() {
	local host IFS=. part
	host="$(printf '%s' "$1" | tr 'A-Z' 'a-z')"
	case "$host" in localhost|"[::1]") return 0 ;; 127.*) ;; *) return 1 ;; esac
	set -f
	set -- $host
	set +f
	[ "$#" -eq 4 ] || return 1
	for part in "$@"; do
		case "$part" in ''|*[!0-9]*) return 1 ;; esac
		[ "${#part}" -le 3 ] && [ "$((10#$part))" -le 255 ] || return 1
	done
	return 0
}

# CITAR_RELEASE_URL: https anywhere, or plain http to this machine only (a test's own server); never with a user name
# in it. Sets CURL_PROTO; refuses before anything is requested.
check_release_url() {
	local url="$1" rest authority host
	case "$url" in
		https://*) CURL_PROTO="=https"; rest="${url#https://}" ;;
		http://*)  CURL_PROTO="=http"; rest="${url#http://}" ;;
		*) die "Refused: CITAR_RELEASE_URL $url is not an https:// address." ;;
	esac
	authority="${rest%%/*}"
	case "$authority" in
		"") die "Refused: CITAR_RELEASE_URL $url names no host." ;;
		*@*) die "Refused: CITAR_RELEASE_URL $url names a user; give the address alone." ;;
		\[*) host="${authority%%]*}]" ;;
		*) host="${authority%%:*}" ;;
	esac
	if [ "$CURL_PROTO" = "=http" ] && ! is_loopback "$host"; then
		die "Refused: CITAR_RELEASE_URL $url is plain http to $host. Plain http is allowed to this machine only
(127.0.0.1, localhost or [::1]); use https."
	fi
}

install_helper() {
	local base url want got helper="citar-helper"
	command -v curl >/dev/null 2>&1 || die "--worker downloads the helper with curl: install curl, then run this again."
	base="${CITAR_RELEASE_URL:-$RELEASES_DEFAULT}"
	base="${base%/}"
	check_release_url "$base"
	if [ -n "$VERSION" ]; then
		url="$base/download/v$VERSION/$HELPER_ASSET"
	else
		url="$base/latest/download/$HELPER_ASSET"
	fi
	HELPER_TMP="$(mktemp -d "${TMPDIR:-/tmp}/citar-helper.XXXXXX")"
	step "Downloading the CITAR helper: $url"
	# Every redirect https (GitHub sends the file from its own storage), so the checksum comes as safely as the file.
	curl -fsSL --proto "$CURL_PROTO" --proto-redir "=https" --retry 2 --connect-timeout 30 \
		-o "$HELPER_TMP/$HELPER_ASSET.sha256" "$url.sha256" || die "Could not download $url.sha256"
	curl -fsSL --proto "$CURL_PROTO" --proto-redir "=https" --retry 2 --connect-timeout 30 \
		-o "$HELPER_TMP/$HELPER_ASSET" "$url" || die "Could not download $url"
	want="$(tr -d '\r' < "$HELPER_TMP/$HELPER_ASSET.sha256" | awk 'NR == 1 { print tolower($1) }')"
	case "$want" in
		*[!0-9a-f]*|"") die "Refused: $url.sha256 does not hold a SHA-256. Nothing was installed." ;;
	esac
	[ "${#want}" -eq 64 ] || die "Refused: $url.sha256 does not hold a SHA-256. Nothing was installed."
	got="$(sha256_of "$HELPER_TMP/$HELPER_ASSET")"
	[ "$got" = "$want" ] || die "Refused: the download's SHA-256 is $got, and $url.sha256 says $want.
Nothing was installed."
	ok "SHA-256 $got, as its .sha256 says"
	mkdir -p "$BIN_DIR"
	cp "$HELPER_TMP/$HELPER_ASSET" "$BIN_DIR/.citar-helper.install"
	chmod 755 "$BIN_DIR/.citar-helper.install"
	# Renamed into place: a helper running from the old file goes on running it.
	mv -f "$BIN_DIR/.citar-helper.install" "$BIN_DIR/citar-helper"
	ok "Installed $BIN_DIR/citar-helper"
	put_on_path
	on_path "$BIN_DIR" || helper="$BIN_DIR/citar-helper"
	say ""
	say "Next, with a worker token from the Servers page of your CITAR server:"
	say "    $helper setup              # the server's address, the token, and whether to lend CPU"
	say "    $helper service install    # keep it running: started at sign-in, and again after an update or a crash"
	say "or run it now in this terminal:  $helper"
	exit 0
}

if [ "$WORKER" -eq 1 ]; then
	install_helper
fi

# ----------------------------------------------------------------------------- python
# Whether $1 can install CITAR's wheel: the reason it cannot, or nothing. A free-threaded build (3.13t, 3.14t) cannot
# load an abi3 extension, so it is told to use the regular build of the same version.
python_problem() {
	local probed version implementation free bits
	probed="$("$1" -c 'import struct, sys, sysconfig; print("%d.%d" % sys.version_info[:2], sys.implementation.name, 1 if sysconfig.get_config_var("Py_GIL_DISABLED") else 0, struct.calcsize("P") * 8)' 2>/dev/null)" \
		|| { printf 'it does not run'; return 0; }
	set -f
	set -- $probed
	set +f
	version="$(test_fact python || printf '%s' "$1")"
	implementation="$(test_fact implementation || printf '%s' "$2")"
	free="$(test_fact free_threaded || printf '%s' "$3")"
	bits="$(test_fact bits || printf '%s' "$4")"
	if ! at_least "$version" "$CITAR_MIN_PYTHON"; then
		printf 'it is Python %s, and CITAR needs %s or newer' "$version" "$CITAR_MIN_PYTHON"
	elif [ "$implementation" != "cpython" ]; then
		printf 'no-prebuilt:%s Python %s (the builds are for CPython)' "$implementation" "$version"
	elif [ "$free" != "0" ]; then
		printf 'free-threaded:%s' "$version"
	elif [ "$bits" != "64" ]; then
		printf 'no-prebuilt:a %s-bit Python (the builds are for 64-bit ones)' "$bits"
	fi
	return 0
}

refuse_python() {   # $1 the interpreter, $2 its problem
	case "$2" in
		no-prebuilt:*) no_prebuilt "${2#no-prebuilt:}" ;;
		free-threaded:*)
			die "$1 is Python ${2#free-threaded:} built free-threaded (${2#free-threaded:}t), which cannot load CITAR's
prebuilt engine: the wheels are for CPython with the GIL. Use the regular build of Python ${2#free-threaded:}
(python${2#free-threaded:}, not python${2#free-threaded:}t), with --python if it is not found first." ;;
		*) die "--python $1: $2." ;;
	esac
}

step "Looking for Python ${CITAR_MIN_PYTHON} or newer"
if [ -n "$PYTHON" ]; then
	command -v "$PYTHON" >/dev/null 2>&1 || die "--python $PYTHON: no such program"
	problem="$(python_problem "$PYTHON")"
	[ -z "$problem" ] || refuse_python "$PYTHON" "$problem"
else
	# Several are tried because the newest Python on a machine is frequently not the one called `python3`; the first
	# that can install CITAR is taken, else the first new enough one says why it cannot.
	first_problem=""
	for candidate in python3.14 python3.13 python3.12 python3.11 python3 python; do
		command -v "$candidate" >/dev/null 2>&1 || continue
		problem="$(python_problem "$candidate")"
		if [ -z "$problem" ]; then
			PYTHON="$candidate"
			break
		fi
		case "$problem" in "it is Python "*|"it does not run") ;; *) [ -n "$first_problem" ] || first_problem="$candidate|$problem" ;; esac
	done
	if [ -z "$PYTHON" ] && [ -n "$first_problem" ]; then
		refuse_python "${first_problem%%|*}" "${first_problem#*|}"
	fi
fi
if [ -z "$PYTHON" ]; then
	say ""
	die "No Python ${CITAR_MIN_PYTHON}+ found.

  Debian / Ubuntu   sudo apt install python3 python3-venv
  Fedora / RHEL     sudo dnf install python3
  Arch              sudo pacman -S python
  macOS             brew install python@3.12   (or install from python.org)

Then run this installer again."
fi
ok "$("$PYTHON" -V 2>&1) at $(command -v "$PYTHON")"

# python3-venv is a separate package on Debian and Ubuntu, and its absence is the single most
# common failure of an installer like this one. Checked here so the message names the package.
if ! "$PYTHON" -c "import venv, ensurepip" >/dev/null 2>&1; then
	die "This Python cannot create virtual environments.
On Debian or Ubuntu:  sudo apt install python3-venv
Then run this installer again."
fi

# ----------------------------------------------------------------------------- the wheel
# PEP 440's spelling of a release's version, as a wheel's name carries it (0.1.6-rc.1 is 0.1.6rc1).
pep440() { printf '%s' "$1" | sed -e 's/-alpha\./a/; s/-beta\./b/; s/-rc\./rc/'; }

no_wheel() {
	die "$1: this installer installs CITAR's prebuilt wheels only.
'pip install' of a source distribution or a checkout builds CITAR from source, with a Rust toolchain
(https://rustup.rs)."
}

WHEEL_DIR=""
if [ -n "$SOURCE" ]; then
	# A local wheel, or a folder of wheels: how CI tests this script before a release exists, and how an offline
	# install works. Never a source distribution or a checkout.
	[ -e "$SOURCE" ] || die "No such file or folder: $SOURCE"
	if [ -d "$SOURCE" ]; then
		WHEEL_DIR="$(cd "$SOURCE" && pwd -P)"
		versions=""
		for wheel in "$WHEEL_DIR"/citar-*.whl; do
			[ -f "$wheel" ] || continue
			v="$(basename "$wheel" | cut -d- -f2)"
			case " $versions " in *" $v "*) ;; *) versions="${versions:+$versions }$v" ;; esac
		done
		if [ -z "$versions" ]; then
			sdists="$(cd "$WHEEL_DIR" && ls citar-*.tar.gz citar-*.zip 2>/dev/null | tr '\n' ' ' || true)"
			no_wheel "$WHEEL_DIR holds no CITAR wheel${sdists:+ (only ${sdists% }, a source distribution)}"
		fi
		if [ -n "$VERSION" ]; then
			case " $versions " in *" $(pep440 "$VERSION") "*) ;; *) die "$WHEEL_DIR holds CITAR $versions, not $VERSION" ;; esac
		else
			case "$versions" in *" "*) die "$WHEEL_DIR holds CITAR $versions: name the one to install with --version" ;; esac
			VERSION="$versions"
		fi
	else
		case "$SOURCE" in
			*.whl) ;;
			*) no_wheel "$SOURCE is not a wheel" ;;
		esac
		SOURCE="$(cd "$(dirname "$SOURCE")" && pwd -P)/$(basename "$SOURCE")"
		if [ -n "$VERSION" ]; then
			[ "$(basename "$SOURCE" | cut -d- -f2)" = "$(pep440 "$VERSION")" ] || die "$SOURCE is not CITAR $VERSION"
		fi
	fi
fi

# ----------------------------------------------------------------------------- install
if [ -e "$PREFIX" ] && ! is_citar_prefix "$PREFIX" && [ -n "$(ls -A "$PREFIX" 2>/dev/null)" ]; then
	die "$PREFIX is not empty and holds no CITAR install: name a new or empty folder with --prefix."
fi
step "Installing CITAR into $PREFIX"
$SUDO mkdir -p "$PREFIX"
# The marker first, so an install that fails half way can be run again (or removed with --uninstall).
printf 'installing with install.sh\n' | $SUDO tee "$PREFIX/.citar-installer" >/dev/null

$SUDO "$PYTHON" -m venv "$PREFIX/venv"
VENV_PIP="$PREFIX/venv/bin/pip"
# A new pip first: the ensurepip of an older distribution (Ubuntu 20.04's pip 20.0) knows no manylinux_2_28 wheel.
$SUDO "$VENV_PIP" install --quiet --disable-pip-version-check --upgrade pip

# Extras by role: a home machine wants every provider client, a server does not need the local
# model libraries.
case "$MODE" in
	server) EXTRAS="server" ;;
	*)      EXTRAS="all" ;;
esac
if [ -n "$WHEEL_DIR" ]; then
	# pip picks the wheel for this machine from the folder alone (no index), then installs it as a file below.
	PICKED="$(mktemp -d "${TMPDIR:-/tmp}/citar-wheel.XXXXXX")"
	$SUDO "$VENV_PIP" download --quiet --disable-pip-version-check --no-deps --no-index --only-binary=:all: \
		--find-links "$WHEEL_DIR" --dest "$PICKED" "citar==$VERSION" \
		|| no_wheel "$WHEEL_DIR holds no CITAR $VERSION wheel for this machine"
	SOURCE="$(ls "$PICKED"/citar-*.whl)"
fi
if [ -n "$SOURCE" ]; then
	SPEC="$SOURCE[$EXTRAS]"
else
	SPEC="citar[$EXTRAS]"
	[ -n "$VERSION" ] && SPEC="citar[$EXTRAS]==$VERSION"
fi

# --only-binary=citar: CITAR from a wheel or not at all (its dependencies as pip chooses). Without it, a machine
# with no wheel would download the source distribution and fail minutes later, building Rust.
step "Downloading CITAR and its dependencies"
$SUDO "$VENV_PIP" install --quiet --disable-pip-version-check --only-binary=citar "$SPEC" \
	|| die "pip could not install $SPEC. If pip found no matching distribution, there is no prebuilt CITAR${VERSION:+ $VERSION}
for this machine: 'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs)."
INSTALLED="$("$PREFIX/venv/bin/citar" --version 2>/dev/null || echo "CITAR")"
printf 'installed by install.sh: %s\nbin: %s\n' "$INSTALLED" "$BIN_DIR" | $SUDO tee "$PREFIX/.citar-installer" >/dev/null
ok "$INSTALLED installed"

# ----------------------------------------------------------------------------- the commands
step "Putting citar on your PATH"
$SUDO mkdir -p "$BIN_DIR"
for command_name in citar citar-admin citar-mcp; do
	[ -x "$PREFIX/venv/bin/$command_name" ] || continue
	$SUDO ln -sf "$PREFIX/venv/bin/$command_name" "$BIN_DIR/$command_name"
done
ok "Linked into $BIN_DIR"
put_on_path

# ----------------------------------------------------------------------------- setup
cleanup
say ""
if [ "$RUN_SETUP" -eq 0 ]; then
	ok "Installed. Run 'citar setup' when you are ready."
	exit 0
fi

case "$MODE" in
	server) SETUP_FLAG="--server" ;;
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
