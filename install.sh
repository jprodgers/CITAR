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
# A server is installed in the release layout (design/phase5-release.md P5.13.1): each release is a venv of its own,
# root's and read-only to the service, in /opt/citar/releases/<version>/venv beside the wheel it came from, its
# pip freeze and its SHA256SUMS line; /opt/citar/current links to the release that runs and is switched atomically.
# `citar setup --server` then writes /etc/citar/citar.env and, if asked, the service (which runs
# /opt/citar/current/venv/bin/citar with /var/lib/citar as its state and working directory), citar-admin and the
# nightly backup. Run again with a newer --version, it installs that release beside the running one and switches to
# it, keeping the configuration: with the service stopped and right after a backup, the unit and scripts written
# again from the new release, and undone (the database put back if it was migrated) if the new release does not
# answer. An older release is refused, and the release that runs is never removed.
#
# Flags:
#   --server            install for a public server (implies a system-wide prefix)
#   --worker            install the CITAR helper (citar-helper) instead of the game
#   --plan              print each step this install would take, with every path, and change nothing
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
PLAN=0

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
		say "Usage: install.sh [--server | --worker] [--plan] [--prefix DIR] [--bin-dir DIR] [--no-profile] [--python PATH]"
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
		--plan)       PLAN=1 ;;
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
if [ "$PLAN" -eq 1 ]; then
	[ "$WORKER" -eq 0 ] || die "--plan is for the game's install, not --worker's download"
	[ "$UNINSTALL" -eq 0 ] || die "--plan is for an install, not --uninstall"
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
BOOT=""
cleanup() {
	[ -z "$HELPER_TMP" ] || rm -rf "$HELPER_TMP"
	[ -z "$PICKED" ] || rm -rf "$PICKED"
	[ -z "$BOOT" ] || rm -rf "$BOOT"
	HELPER_TMP=""; PICKED=""; BOOT=""
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
if [ "$MODE" = "server" ] && [ "$(id -u)" -ne 0 ] && [ "$PLAN" -eq 0 ]; then
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
	local i x y
	local -a a b
	IFS=. read -r -a a <<< "$1"
	IFS=. read -r -a b <<< "$2"
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
# service that cannot start, so its own uninstall comes first. `service install` names the path the helper found for
# itself, which on Linux has every symbolic link resolved (/proc/self/exe): so a bin folder reached through a link
# (a dotfile manager's ~/.local/bin) is looked for in both its forms.
helper_service() {
	local definition real
	real="$(cd "$BIN_DIR" 2>/dev/null && pwd -P)" || real="$BIN_DIR"
	for definition in "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/citar-helper.service" \
		/etc/systemd/system/citar-helper.service "$HOME/Library/LaunchAgents/org.citar.helper.plist"; do
		if [ -f "$definition" ] && grep -qF -e "$BIN_DIR/citar-helper" -e "$real/citar-helper" "$definition" 2>/dev/null; then
			printf '%s' "$definition"
			return 0
		fi
	done
	return 1
}

# A prefix this script (or 0.1.5's) installed: its marker, the game's venv, or a server's current release. Nothing else
# is ever deleted.
is_citar_prefix() { [ -f "$1/.citar-installer" ] || [ -x "$1/venv/bin/citar" ] || [ -x "$1/current/venv/bin/citar" ]; }

# What `citar setup --server` writes beside the unit, recognised by a line of its own so that a file of the same name
# that is not CITAR's is never removed.
SERVER_FILES="/usr/local/bin/citar-admin|citar-admin: CITAR's administration commands
/usr/local/bin/citar-backup|Back up CITAR: the database, the whole state directory"

if [ "$UNINSTALL" -eq 1 ]; then
	step "Removing CITAR"
	if [ -e "$BIN_DIR/citar-helper" ] && service="$(helper_service)"; then
		die "The helper at $BIN_DIR/citar-helper runs as a service ($service). Remove the service first:
    $BIN_DIR/citar-helper service uninstall      (add --system for the machine's)
then run this again. Nothing was removed."
	fi
	if [ "$WORKER" -eq 0 ]; then
		if [ "$MODE" = "server" ] && command -v systemctl >/dev/null 2>&1; then
			$SUDO systemctl disable --now citar-backup.timer 2>/dev/null || true
			$SUDO systemctl disable --now citar 2>/dev/null || true
			$SUDO rm -f /etc/systemd/system/citar.service /etc/systemd/system/citar-backup.service \
				/etc/systemd/system/citar-backup.timer
			$SUDO systemctl daemon-reload 2>/dev/null || true
		fi
		if [ "$MODE" = "server" ]; then
			while IFS='|' read -r file mark; do
				if [ -f "$file" ] && grep -qF "$mark" "$file" 2>/dev/null; then
					$SUDO rm -f "$file"
					ok "Removed $file."
				fi
			done <<< "$SERVER_FILES"
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
	# The binary and what its updates leave beside it (crates/citar-helper/src/update/files.rs): the binary the last
	# update replaced (.old), one rolled back while it ran (.failed), either set aside under a process id (.old.<pid>,
	# .failed.<pid>), an update's half-written file, and this script's own. The helper clears these at its next start,
	# and after an uninstall there is none.
	removed=0
	for file in "$BIN_DIR/citar-helper" "$BIN_DIR/citar-helper.old" "$BIN_DIR/citar-helper.failed" \
		"$BIN_DIR/.citar-helper.new" "$BIN_DIR/.citar-helper.install" \
		"$BIN_DIR"/citar-helper.old.* "$BIN_DIR"/citar-helper.failed.*; do
		[ -e "$file" ] || [ -L "$file" ] || continue
		case "${file##*/}" in
			citar-helper.old.*|citar-helper.failed.*)
				case "${file##*.}" in ""|*[!0-9]*) continue ;; esac ;;
		esac
		$SUDO rm -f "$file"
		removed=1
	done
	[ "$removed" -eq 0 ] || ok "Removed citar-helper and what its updates left from $BIN_DIR."
	say ""
	say "Your games, settings and results were NOT deleted. They are in:"
	say "  ${XDG_DATA_HOME:-$HOME/.local/share}/citar   (or /var/lib/citar for a server, its configuration in /etc/citar)"
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
# By its path from here on: sudo's PATH (secure_path) may not hold the folder it was found in.
PYTHON="$(command -v "$PYTHON")"

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

# Extras by role: a home machine wants every provider client, a server does not need the local
# model libraries.
case "$MODE" in
	server) EXTRAS="server" ;;
	*)      EXTRAS="all" ;;
esac
case "$MODE" in
	server) SETUP_FLAG="--server" ;;
	*)      SETUP_FLAG="--local" ;;
esac

# What will be installed, as far as it is known before pip has looked: the wheel named, the release asked for, or the
# newest on PyPI.
if [ -n "$SOURCE" ]; then
	WHEEL_DESC="$SOURCE"
	PLAN_VERSION="$(basename "$SOURCE" | cut -d- -f2)"
elif [ -n "$WHEEL_DIR" ]; then
	WHEEL_DESC="this machine's CITAR $VERSION wheel from $WHEEL_DIR (its dependencies from PyPI)"
	PLAN_VERSION="$(pep440 "$VERSION")"
elif [ -n "$VERSION" ]; then
	WHEEL_DESC="citar[$EXTRAS]==$VERSION from PyPI, a prebuilt wheel only"
	PLAN_VERSION="$(pep440 "$VERSION")"
else
	WHEEL_DESC="citar[$EXTRAS], the newest release on PyPI, a prebuilt wheel only"
	PLAN_VERSION="<version>"
fi
PYTHON_DESC="$("$PYTHON" -V 2>&1) at $(command -v "$PYTHON")"

# ----------------------------------------------------------------------------- a server: the release layout
RELEASES="$PREFIX/releases"
CURRENT="$PREFIX/current"
# The release that runs now: the folder $CURRENT links to, or nothing.
CURRENT_VERSION=""
if [ -L "$CURRENT" ]; then
	CURRENT_VERSION="$(readlink "$CURRENT")"
	CURRENT_VERSION="${CURRENT_VERSION%/}"
	CURRENT_VERSION="${CURRENT_VERSION##*/}"
fi
# Where citar-backup puts its folders (its default, deploy/citar-backup.sh), and how long a switched release has to
# answer. CITAR_DEPLOY_ROOT (tests only) prefixes the backups and the database a backup names, as it prefixes every
# path in citar-backup itself; CITAR_INSTALL_HEALTH_SECONDS (tests only) shortens the wait.
DEPLOY_ROOT="${CITAR_DEPLOY_ROOT:-}"
BACKUPS="$DEPLOY_ROOT/var/backups/citar"
HEALTH_SECONDS="${CITAR_INSTALL_HEALTH_SECONDS:-60}"

# Whether PEP 440 version $1 is older than $2 (CITAR's own: X.Y.Z with an optional aN, bN or rcN); exit 2 when either
# is not such a version.
older_than() {
	"$PYTHON" - "$1" "$2" <<'PY'
import re
import sys


def key(version):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:(a|b|rc)(\d+))?", version)
    if not m:
        sys.exit(2)
    return (int(m[1]), int(m[2]), int(m[3]), {"a": 0, "b": 1, "rc": 2}.get(m[4], 3), int(m[5] or 0))


sys.exit(0 if key(sys.argv[1]) < key(sys.argv[2]) else 1)
PY
}

# Refuses release $1 when it is older than the one that runs, before anything changes (the run and --plan alike).
refuse_older() {
	local order=0
	if [ -z "$CURRENT_VERSION" ] || [ "$CURRENT_VERSION" = "$1" ]; then
		return 0
	fi
	older_than "$1" "$CURRENT_VERSION" || order=$?
	case "$order" in
		0) die "CITAR $1 is older than the release that runs ($CURRENT -> releases/$CURRENT_VERSION). This script never
takes a server back: the running release has migrated the database, which $1 cannot open. Nothing was changed." ;;
		2) die "Cannot compare $1 with the release that runs ($CURRENT -> releases/$CURRENT_VERSION). Nothing was changed." ;;
	esac
}

# Whether release folder $1 is the one $CURRENT resolves to.
is_current() {
	[ -L "$CURRENT" ] && [ -d "$1" ] || return 1
	[ "$(cd "$1" && pwd -P)" = "$(cd "$CURRENT" && pwd -P)" ]
}

# Points $CURRENT at releases/$1 in one rename, so there is no moment without a current release (ln -sfn removes the
# old link first).
switch_current() {
	$SUDO "$PYTHON" -c 'import os, sys
link, target = sys.argv[1], sys.argv[2]
staged = link + ".switching"
if os.path.lexists(staged):
    os.remove(staged)
os.symlink(target, staged)
os.replace(staged, link)' "$CURRENT" "releases/$1"
}

mark_prefix() {   # $1 the release that is current
	printf 'installed by install.sh: CITAR %s (%s)\nbin: %s\n' "$1" "$CURRENT" "$BIN_DIR" \
		| $SUDO tee "$PREFIX/.citar-installer" >/dev/null
}

# citar and citar-mcp, linked through current so they follow a switch. citar-admin is not linked: the venv's own
# command would run as root with no configuration (so on a database of its own); `citar setup --server` installs the
# wrapper that runs it as the service runs.
link_commands() {
	step "Putting citar on the PATH"
	$SUDO mkdir -p "$BIN_DIR"
	for command_name in citar citar-mcp; do
		$SUDO ln -sfn "$CURRENT/venv/bin/$command_name" "$BIN_DIR/$command_name"
	done
	ok "Linked into $BIN_DIR"
	put_on_path
}

# The citar service's unit as systemd has it (its drop-ins after it), or nothing when none is installed.
service_unit() {
	command -v systemctl >/dev/null 2>&1 || return 0
	$SUDO systemctl cat citar.service 2>/dev/null || true
}

# The last value of directive $2 in the unit text $1 (a drop-in's comes after the unit's own), without the - that
# makes a file optional.
unit_value() {
	local value
	value="$(printf '%s\n' "$1" | sed -n "s/^[[:space:]]*$2[[:space:]]*=[[:space:]]*//p" | tail -n 1)"
	printf '%s' "${value#-}"
}

# Whether the server answers /api/meta as CITAR $1 within HEALTH_SECONDS; prints what it last saw. Its address is
# CITAR_HOST and CITAR_PORT from env file $2, read as systemd reads it (the last assignment wins), else the server's
# defaults. Never through a proxy.
healthy() {
	$SUDO "$PYTHON" -I - "$1" "$2" "$HEALTH_SECONDS" <<'PY'
import json
import sys
import time
import urllib.request

want, env_file, seconds = sys.argv[1], sys.argv[2], float(sys.argv[3])
values = {}
try:
    with open(env_file, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line[0] in "#;" or "=" not in line:
                continue
            key, _, value = line.partition("=")
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            values[key.strip()] = value
except OSError:
    pass
host = values.get("CITAR_HOST") or "127.0.0.1"
host = {"0.0.0.0": "127.0.0.1", "::": "::1"}.get(host, host)
if ":" in host:
    host = f"[{host}]"
url = f"http://{host}:{values.get('CITAR_PORT') or '8765'}/api/meta"
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def pep440(version):
    return version.replace("-alpha.", "a").replace("-beta.", "b").replace("-rc.", "rc")


deadline = time.monotonic() + seconds
seen = f"{url}: no answer"
while True:
    try:
        with opener.open(url, timeout=5) as response:
            got = str(json.load(response).get("version", ""))
        if pep440(got) == want:
            print(f"{url} answers as CITAR {got}")
            sys.exit(0)
        seen = f"{url} answers as CITAR {got}, not {want}"
    except Exception as exc:
        seen = f"{url}: {exc}"
    if time.monotonic() >= deadline:
        print(seen)
        sys.exit(1)
    time.sleep(1)
PY
}

# The alembic revision of the database at $1 (opened read-only), or "unknown".
db_revision() {
	$SUDO "$PYTHON" -I -c 'import pathlib, sqlite3, sys
try:
    db = sqlite3.connect(pathlib.Path(sys.argv[1]).as_uri() + "?mode=ro", uri=True)
    print(db.execute("SELECT version_num FROM alembic_version").fetchone()[0])
except Exception:
    print("unknown")' "$1"
}

# Puts snapshot $2 back as the database $1 in one rename, with the owner and mode $1 had, its write-ahead log and
# journal removed (they belong to the database being replaced).
restore_db() {
	$SUDO "$PYTHON" -I -c 'import os, shutil, sys
db, snapshot = sys.argv[1], sys.argv[2]
info = os.stat(db)
staged = db + ".citar-restore"
shutil.copyfile(snapshot, staged)
os.chown(staged, info.st_uid, info.st_gid)
os.chmod(staged, info.st_mode & 0o7777)
for suffix in ("-wal", "-shm", "-journal"):
    if os.path.lexists(db + suffix):
        os.remove(db + suffix)
os.replace(staged, db)' "$1" "$2"
}

# The switch's backup, with the service stopped: citar-backup.service is a oneshot, so starting it waits for it. Sets
# BACKUP to the new folder, or returns 1 when there is no trustworthy one. Exit 3 is a whole backup whose offsite copy
# failed: a switch may use it.
BACKUP=""
take_backup() {
	local before status=0 newest
	before="$(date -u +%Y%m%d-%H%M%S)"
	$SUDO systemctl start citar-backup || status=$?
	if [ "$status" -ne 0 ]; then
		[ "$($SUDO systemctl show -p ExecMainStatus --value citar-backup 2>/dev/null)" = "3" ] || return 1
		warn "The backup's offsite copy failed (journalctl -u citar-backup); the backup on this disk is whole."
	fi
	newest="$($SUDO ls -1 "$BACKUPS" 2>/dev/null | grep -E '^[0-9]{8}-[0-9]{6}(-[0-9]+)?$' | sort | tail -n 1 || true)"
	[ -n "$newest" ] || return 1
	[[ ! "$newest" < "$before" ]] || return 1
	BACKUP="$BACKUPS/$newest"
	$SUDO test -f "$BACKUP/RELEASE" && ! $SUDO test -e "$BACKUP/UNTRUSTWORTHY"
}

# A newer release while one runs. With the citar service installed, P5.13.3's switch: the service stopped, a backup
# taken, current switched, the unit and scripts written again from the new release, and the service started; a release
# that does not answer as itself is undone, its database put back from that backup if it moved the revision. Without a
# service nothing runs from the layout here, so current is switched alone.
upgrade_server() {   # $1 the new release, installed and complete
	local p="$1" old="$CURRENT_VERSION" unit env_file info before_rev after_rev database said="" back restored="no"
	unit="$(service_unit)"
	if [ -z "$unit" ]; then
		switch_current "$p"
		ok "$CURRENT -> releases/$p; $old stays in $RELEASES/$old."
		mark_prefix "$p"
		link_commands
		cleanup
		say ""
		say "No citar service is installed here, so nothing was stopped or started: CITAR runs $p from $CURRENT the next"
		say "time it starts. It migrates the database then, after which $old cannot open it."
		exit 0
	fi
	env_file="$(unit_value "$unit" EnvironmentFile)"
	step "Switching the citar service from $old to $p: stopped, backed up, switched, started"
	$SUDO systemctl stop citar
	if ! take_backup; then
		$SUDO systemctl start citar || true
		die "The backup before the switch failed or is not trustworthy (journalctl -u citar-backup), so nothing was
switched and citar was started again on $old. $p is installed in $RELEASES/$p: run this again once the backup works."
	fi
	ok "Backed up into $BACKUP"
	info="$($SUDO cat "$BACKUP/RELEASE")"
	before_rev="$(printf '%s\n' "$info" | sed -n 's/^revision=//p' | head -n 1)"
	database="$(printf '%s\n' "$info" | sed -n 's/^database=//p' | head -n 1)"
	switch_current "$p"
	ok "$CURRENT -> releases/$p"
	if $SUDO "$CURRENT/venv/bin/citar" setup --server --refresh-units && $SUDO systemctl daemon-reload \
		&& $SUDO systemctl start citar && said="$(healthy "$p" "$env_file")"; then
		ok "$said"
		mark_prefix "$p"
		link_commands
		cleanup
		say ""
		ok "CITAR $p runs. $old stays in $RELEASES/$old, and the backup taken before the switch is $BACKUP."
		exit 0
	fi
	warn "CITAR $p did not come up${said:+ ($said)}: undoing the switch."
	$SUDO systemctl stop citar || true
	case "$before_rev" in
		""|unknown|none) ;;
		*)
			case "$database" in
				/*)
					after_rev="$(db_revision "$DEPLOY_ROOT$database")"
					if [ "$after_rev" != "$before_rev" ] && $SUDO test -f "$BACKUP/citar.db"; then
						restore_db "$DEPLOY_ROOT$database" "$BACKUP/citar.db"
						restored="yes, from $BACKUP/citar.db ($p had moved its revision from $before_rev to $after_rev)"
					fi ;;
			esac ;;
	esac
	switch_current "$old"
	$SUDO "$CURRENT/venv/bin/citar" setup --server --refresh-units \
		|| warn "$old could not write its own unit and scripts again: $p's are in place."
	$SUDO systemctl daemon-reload || true
	$SUDO systemctl start citar || true
	if back="$(healthy "$old" "$env_file")"; then
		back="citar runs $old again ($back)"
	else
		back="citar did not come back on $old either ($back): see journalctl -u citar"
	fi
	die "CITAR $p was not switched to, because it did not answer as $p. $CURRENT -> releases/$old again, and $back.
The database was restored: $restored. The backup is $BACKUP; $p stays in $RELEASES/$p."
}

plan_setup() {   # the wizard's part of a plan; then the plan ends
	if [ "$RUN_SETUP" -eq 1 ]; then
		say "Then $CURRENT/venv/bin/citar setup --server writes, after showing them and asking (its defaults):"
	else
		say "Then nothing (--no-setup). '$CURRENT/venv/bin/citar setup --server' writes, after asking (its defaults):"
	fi
	say "  state       /var/lib/citar: citar:citar 0750, the service's state and working directory"
	say "  config      /etc/citar/citar.env: root:citar 0640"
	say "  service     /etc/systemd/system/citar.service: ExecStart=$CURRENT/venv/bin/citar serve"
	say "  admin       /usr/local/bin/citar-admin: each command run as the service runs"
	say "  backup      /usr/local/bin/citar-backup, with citar-backup.service and citar-backup.timer (nightly)"
	exit 0
}

# The steps install_server takes, as far as they are known before pip has chosen the wheel.
plan_server() {
	local p="$PLAN_VERSION" release unit
	release="$RELEASES/$p"
	[ "$p" = "<version>" ] || refuse_older "$p"
	say "The plan (nothing is changed; run it without --plan to do it):"
	say "  python      $PYTHON_DESC"
	say "  wheel       $WHEEL_DESC"
	say "  as          root$( [ "$(id -u)" -eq 0 ] || printf ' (through sudo)')"
	if [ -n "$CURRENT_VERSION" ] && [ "$p" = "$CURRENT_VERSION" ]; then
		say "CITAR $p is the release that runs, so the layout stays as it is:"
		if [ -f "$release/.complete" ]; then
			say "  release     $release: installed already"
		else
			say "  release     $release: marked complete if its own citar --version says $p, else refused (the"
			say "              release that runs is never made again)"
		fi
		say "  current     $CURRENT -> releases/$p, unchanged"
		say "  commands    $BIN_DIR/citar and $BIN_DIR/citar-mcp -> $CURRENT/venv/bin/"
		plan_setup
	fi
	say "This script makes the release layout:"
	say "  prefix      $PREFIX, marked by $PREFIX/.citar-installer"
	say "  release     $release/venv: a venv of its own, root's, read-only to the service"
	say "              $release/: the wheel, freeze.txt and SHA256SUMS beside it, then .complete"
	if [ -z "$CURRENT_VERSION" ]; then
		say "  current     $CURRENT -> releases/$p, switched atomically once it is installed"
		say "  commands    $BIN_DIR/citar and $BIN_DIR/citar-mcp -> $CURRENT/venv/bin/"
		plan_setup
	fi
	say "  running     $CURRENT -> releases/$CURRENT_VERSION, kept beside the new release"
	if [ "$p" = "<version>" ]; then
		say "  (if the newest is older than $CURRENT_VERSION it is refused; if it is $CURRENT_VERSION it is kept and the"
		say "  setup runs; if it is newer, as follows)"
	fi
	unit="$(service_unit)"
	if [ -z "$unit" ]; then
		say "  current     $CURRENT -> releases/$p, switched atomically once it is installed"
		say "  commands    $BIN_DIR/citar and $BIN_DIR/citar-mcp -> $CURRENT/venv/bin/"
		say "and nothing more: no citar service is installed here, so nothing is stopped or started."
		exit 0
	fi
	say "The citar service is installed, so the switch is made with it stopped:"
	say "  1. systemctl stop citar"
	say "  2. systemctl start citar-backup: a new, trustworthy backup in $BACKUPS (else citar starts again"
	say "     on $CURRENT_VERSION and nothing is switched)"
	say "  3. current     $CURRENT -> releases/$p, switched atomically"
	say "  4. $CURRENT/venv/bin/citar setup --server --refresh-units: the unit, citar-admin and the backup from $p"
	say "  5. systemctl daemon-reload, then systemctl start citar"
	say "  6. /api/meta (CITAR_HOST and CITAR_PORT in $(unit_value "$unit" EnvironmentFile)) answers as CITAR $p within ${HEALTH_SECONDS} s"
	say "  7. commands    $BIN_DIR/citar and $BIN_DIR/citar-mcp -> $CURRENT/venv/bin/"
	say "If $p does not answer: citar is stopped, the database is put back from step 2's backup if $p moved its"
	say "revision, current goes back to releases/$CURRENT_VERSION (its unit and scripts written again), citar starts"
	say "on $CURRENT_VERSION, and the run exits 1."
	exit 0
}

install_server() {
	local wheel p release staged digest said
	BOOT="$(mktemp -d "${TMPDIR:-/tmp}/citar-server.XXXXXX")"
	# The release's folder is named by the version pip chooses, and a venv cannot be moved once it is made: so the
	# wheel is chosen first, by pip in a venv of its own here, then installed as a file.
	step "Choosing the wheel"
	"$PYTHON" -m venv "$BOOT/venv"
	"$BOOT/venv/bin/pip" install --quiet --disable-pip-version-check --upgrade pip
	mkdir "$BOOT/wheel"
	if [ -n "$SOURCE" ]; then
		cp "$SOURCE" "$BOOT/wheel/"
	elif [ -n "$WHEEL_DIR" ]; then
		"$BOOT/venv/bin/pip" download --quiet --disable-pip-version-check --no-deps --no-index --only-binary=:all: \
			--find-links "$WHEEL_DIR" --dest "$BOOT/wheel" "citar==$VERSION" \
			|| no_wheel "$WHEEL_DIR holds no CITAR $VERSION wheel for this machine"
	else
		"$BOOT/venv/bin/pip" download --quiet --disable-pip-version-check --no-deps --only-binary=:all: \
			--dest "$BOOT/wheel" "citar${VERSION:+==$VERSION}" \
			|| die "pip found no prebuilt CITAR${VERSION:+ $VERSION} for this machine: 'pip install citar' builds it from source,
with a Rust toolchain (https://rustup.rs)."
	fi
	wheel="$(ls "$BOOT"/wheel/citar-*.whl)"
	p="$(basename "$wheel" | cut -d- -f2)"
	release="$RELEASES/$p"
	ok "CITAR $p: $(basename "$wheel")"
	refuse_older "$p"

	$SUDO mkdir -p "$RELEASES"
	# The marker first, so an install that fails half way can be run again (or removed with --uninstall).
	[ -f "$PREFIX/.citar-installer" ] || printf 'installing with install.sh\n' | $SUDO tee "$PREFIX/.citar-installer" >/dev/null
	if [ -f "$release/.complete" ]; then
		ok "CITAR $p is already installed in $release"
	elif is_current "$release"; then
		# The release that runs, without the mark (a release citar-deploy or a hand made, or one marked before 0.1.6's
		# installer): never removed under the running server. Its own citar says whether it is whole.
		said="$("$release/venv/bin/citar" --version 2>/dev/null || true)"
		if [ "$(pep440 "${said#CITAR }")" != "$p" ]; then
			die "$release is the release that runs ($CURRENT), but it has no .complete and its citar --version says
'${said:-nothing}', not CITAR $p. This script never removes the release that runs: point current at a whole release
first (citar-deploy rollback), then run this again. Nothing was changed."
		fi
		printf 'marked by install.sh: %s, the release that runs\n' "$said" | $SUDO tee "$release/.complete" >/dev/null
		ok "CITAR $p is the release that runs ($said): kept as it is, and marked complete"
	else
		# A release folder without its .complete, and not current, is what an install that failed half way left: made
		# again whole.
		[ ! -e "$release" ] || $SUDO rm -rf "$release"
		step "Installing CITAR $p into $release"
		$SUDO mkdir -p "$release"
		$SUDO "$PYTHON" -m venv "$release/venv"
		# A new pip first: the ensurepip of an older distribution (Ubuntu 20.04's pip 20.0) knows no manylinux_2_28 wheel.
		$SUDO "$release/venv/bin/pip" install --quiet --disable-pip-version-check --no-cache-dir --upgrade pip
		staged="$release/$(basename "$wheel")"
		$SUDO cp "$wheel" "$staged"
		step "Downloading CITAR's dependencies"
		$SUDO "$release/venv/bin/pip" install --quiet --disable-pip-version-check --no-cache-dir --only-binary=citar \
			--prefer-binary "${staged}[$EXTRAS]" \
			|| die "pip could not install $(basename "$wheel")[$EXTRAS] into $release/venv."
		$SUDO "$release/venv/bin/pip" freeze --disable-pip-version-check | $SUDO tee "$release/freeze.txt" >/dev/null
		digest="$(sha256_of "$staged")"
		printf '%s  %s\n' "$digest" "$(basename "$wheel")" | $SUDO tee "$release/SHA256SUMS" >/dev/null
		printf 'installed by install.sh: %s\n' "$("$release/venv/bin/citar" --version 2>/dev/null || echo "CITAR $p")" \
			| $SUDO tee "$release/.complete" >/dev/null
		ok "$("$release/venv/bin/citar" --version 2>/dev/null || echo "CITAR $p") installed in $release"
	fi
	if [ -n "$CURRENT_VERSION" ] && [ "$CURRENT_VERSION" != "$p" ]; then
		upgrade_server "$p"
	fi
	switch_current "$p"
	ok "$CURRENT -> releases/$p"
	mark_prefix "$p"
	link_commands
	cleanup
	run_setup "$CURRENT/venv/bin/citar"
}

# Hands over to `citar setup`, or says how to run it.
run_setup() {
	say ""
	if [ "$RUN_SETUP" -eq 0 ]; then
		if [ "$MODE" = "server" ]; then
			ok "Installed. Run 'sudo $1 setup --server' when you are ready."
		else
			ok "Installed. Run 'citar setup' when you are ready."
		fi
		exit 0
	fi
	# `curl … | bash` leaves standard input attached to the pipe the script itself came down, which is
	# already at end of file. An interactive wizard reading that gets EOF on its first question and
	# exits looking cancelled. Re-attaching to the terminal is what makes the one-line install work;
	# where there is no terminal at all — CI, a Dockerfile, a provisioning script — the wizard is not
	# run and the command to run later is printed instead.
	if [ -t 0 ]; then
		step "Setting up"
		exec $SUDO "$1" setup "$SETUP_FLAG"
	elif [ -r /dev/tty ]; then
		step "Setting up"
		exec $SUDO "$1" setup "$SETUP_FLAG" < /dev/tty
	else
		ok "Installed."
		say ""
		say "There is no terminal here, so setup was not run. When you are ready:"
		say "    $SUDO${SUDO:+ }$1 setup $SETUP_FLAG"
		say "or, to configure it without any questions:"
		say "    $SUDO${SUDO:+ }$1 setup $SETUP_FLAG --non-interactive"
	fi
	exit 0
}

plan_local() {
	say "The plan (nothing is changed; run it without --plan to do it):"
	say "  python      $PYTHON_DESC"
	say "  wheel       $WHEEL_DESC"
	say "  prefix      $PREFIX              marked by $PREFIX/.citar-installer"
	say "  venv        $PREFIX/venv"
	say "  commands    $BIN_DIR/citar, $BIN_DIR/citar-admin, $BIN_DIR/citar-mcp -> $PREFIX/venv/bin/"
	if on_path "$BIN_DIR"; then
		say "  PATH        $BIN_DIR is on it already"
	elif [ "$NO_PROFILE" -eq 1 ]; then
		say "  PATH        unchanged (--no-profile)"
	else
		say "  PATH        a line in your shell's profile adds $BIN_DIR"
	fi
	if [ "$RUN_SETUP" -eq 1 ]; then
		say "  then        $PREFIX/venv/bin/citar setup --local"
	else
		say "  then        nothing (--no-setup)"
	fi
	exit 0
}

if [ "$PLAN" -eq 1 ]; then
	[ "$MODE" = "server" ] && plan_server
	plan_local
fi
if [ "$MODE" = "server" ]; then
	install_server
fi
# ----------------------------------------------------------------------------- a personal install
step "Installing CITAR into $PREFIX"
mkdir -p "$PREFIX"
# The marker first, so an install that fails half way can be run again (or removed with --uninstall).
printf 'installing with install.sh\n' > "$PREFIX/.citar-installer"

"$PYTHON" -m venv "$PREFIX/venv"
VENV_PIP="$PREFIX/venv/bin/pip"
# A new pip first: the ensurepip of an older distribution (Ubuntu 20.04's pip 20.0) knows no manylinux_2_28 wheel.
"$VENV_PIP" install --quiet --disable-pip-version-check --upgrade pip

if [ -n "$WHEEL_DIR" ]; then
	# pip picks the wheel for this machine from the folder alone (no index), then installs it as a file below.
	PICKED="$(mktemp -d "${TMPDIR:-/tmp}/citar-wheel.XXXXXX")"
	"$VENV_PIP" download --quiet --disable-pip-version-check --no-deps --no-index --only-binary=:all: \
		--find-links "$WHEEL_DIR" --dest "$PICKED" "citar==$VERSION" \
		|| no_wheel "$WHEEL_DIR holds no CITAR $VERSION wheel for this machine"
	SOURCE="$(ls "$PICKED"/citar-*.whl)"
fi
if [ -n "$SOURCE" ]; then
	SPEC="${SOURCE}[$EXTRAS]"
else
	SPEC="citar[$EXTRAS]"
	[ -n "$VERSION" ] && SPEC="citar[$EXTRAS]==$VERSION"
fi

# --only-binary=citar: CITAR from a wheel or not at all. Without it, a machine with no wheel would download the
# source distribution and fail minutes later, building Rust. --prefer-binary: each dependency at its newest version
# with a wheel for this machine, not a newer source distribution (cryptography has shipped no macOS x86_64 wheel
# since 49.0, and its source needs Rust too).
step "Downloading CITAR and its dependencies"
"$VENV_PIP" install --quiet --disable-pip-version-check --only-binary=citar --prefer-binary "$SPEC" \
	|| die "pip could not install $SPEC. If pip found no matching distribution, there is no prebuilt CITAR${VERSION:+ $VERSION}
for this machine: 'pip install citar' builds it from source, with a Rust toolchain (https://rustup.rs)."
INSTALLED="$("$PREFIX/venv/bin/citar" --version 2>/dev/null || echo "CITAR")"
printf 'installed by install.sh: %s\nbin: %s\n' "$INSTALLED" "$BIN_DIR" > "$PREFIX/.citar-installer"
ok "$INSTALLED installed"

# ----------------------------------------------------------------------------- the commands
step "Putting citar on your PATH"
mkdir -p "$BIN_DIR"
for command_name in citar citar-admin citar-mcp; do
	[ -x "$PREFIX/venv/bin/$command_name" ] || continue
	ln -sf "$PREFIX/venv/bin/$command_name" "$BIN_DIR/$command_name"
done
ok "Linked into $BIN_DIR"
put_on_path

# ----------------------------------------------------------------------------- setup
cleanup
run_setup "$PREFIX/venv/bin/citar"
