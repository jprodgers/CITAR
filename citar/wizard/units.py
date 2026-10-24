"""The server's unit and scripts, as ``citar setup --server`` installs them (design/phase5-release.md P5.13).

They are ``deploy/``'s files: ``citar.service``, the admin wrapper ``citar-admin.sh`` and the backup
(``citar-backup.sh`` with its service and timer), which citar-deploy installs on a server from a deploy
bundle. A wheel carries nothing from outside the package, so the package holds a copy of each in
``citar/wizard/deploy/``, byte for byte, and tests/test_deploy.py holds the copies equal to ``deploy/``.
The wizard writes them with this server's paths put in (the environment file, the state directory, and
the commands of this install), and with the release layout's own paths it writes them unchanged. So
there is one definition of the unit, its sandbox and the wrapper that shares it, whichever way a server
was installed.

This replaces the wizard's own unit (``citar/wizard/server.py:348-382`` before 0.1.6), a second definition
that had drifted from ``deploy/citar.service``: ``Type=simple``, no working directory, a shorter sandbox,
no ``MemoryHigh`` and no CPU or IO weights.
"""
from __future__ import annotations

import re
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path, PurePath, PurePosixPath
from typing import Optional

#: The package's copies of deploy/'s files.
DEPLOY = Path(__file__).resolve().parent / "deploy"
FILES = ("citar.service", "citar-admin.sh", "citar-backup.sh", "citar-backup.service", "citar-backup.timer")

#: The release layout's paths (P5.13.1), as deploy/'s files name them: a Linux server's, whatever runs this.
LAYOUT = PurePosixPath("/opt/citar")
ENV_FILE = PurePosixPath("/etc/citar/citar.env")
STATE_DIR = PurePosixPath("/var/lib/citar")
DOCUMENTATION = "https://citar.example.com"

#: Where each file goes on a server, as citar-deploy installs them.
TARGETS = {
    "citar.service": PurePosixPath("/etc/systemd/system/citar.service"),
    "citar-admin.sh": PurePosixPath("/usr/local/bin/citar-admin"),
    "citar-backup.sh": PurePosixPath("/usr/local/bin/citar-backup"),
    "citar-backup.service": PurePosixPath("/etc/systemd/system/citar-backup.service"),
    "citar-backup.timer": PurePosixPath("/etc/systemd/system/citar-backup.timer"),
}

# A path a unit file and a bash assignment take as it is: no space (a unit would split it), no quote, `$` or
# backslash (bash), and no `%` (a unit's specifiers).
_PLAIN_PATH = re.compile(r"/[A-Za-z0-9._+@=,:/-]*")


def posix(path: PurePath | str) -> str:
    """``path`` as a unit file and a shell script on the server write it."""
    return path.as_posix() if isinstance(path, PurePath) else str(path)


def path_problem(path: PurePath | str) -> Optional[str]:
    """Why ``path`` cannot be written into the unit or the scripts, or None when it can."""
    text = posix(path)
    if not text.startswith("/"):
        return f"{text} is not an absolute path"
    if not _PLAIN_PATH.fullmatch(text):
        return (f"{text} has a character a systemd unit or a shell script would read differently "
                "(a space, a quote, $, % or a backslash)")
    return None


@dataclass(frozen=True)
class Install:
    """What the files are written with: this server's configuration and state, and this copy's commands."""

    env_file: PurePath = ENV_FILE
    state_dir: PurePath = STATE_DIR
    #: The command citar.service starts with `serve` after it: a console script, or `python -P -m citar`.
    citar: str = posix(LAYOUT / "current" / "venv" / "bin" / "citar")
    #: The citar-admin console script the wrapper runs, or None when this copy has none (no wrapper then).
    citar_admin: Optional[str] = posix(LAYOUT / "current" / "venv" / "bin" / "citar-admin")
    #: The release layout's `current` link, which the backup names the release by.
    current: PurePath = LAYOUT / "current"
    documentation: str = DOCUMENTATION

    def problems(self) -> list[str]:
        """Every path that cannot go into the files as it is."""
        found = [path_problem(self.env_file), path_problem(self.state_dir), path_problem(self.current)]
        found += [path_problem(word) for word in self.citar.split(" ") if word.startswith("/")]
        if self.citar_admin is not None:
            found.append(path_problem(self.citar_admin))
        return [problem for problem in found if problem]


def this_install(env_file: PurePath, state_dir: PurePath, documentation: str) -> Install:
    """The files' paths for the copy of CITAR that is running: the release layout's when it runs from one
    (``<root>/releases/<version>/venv``, with ``<root>/current`` linking to that release), so the unit and
    the wrapper follow ``current`` when a deploy switches it; else this venv's own console scripts."""
    venv = Path(sys.prefix).resolve()
    release = venv.parent
    if venv.name == "venv" and release.parent.name == "releases":
        current = release.parent.parent / "current"
        try:
            in_layout = current.resolve(strict=True) == release
        except OSError:
            in_layout = False
        if in_layout:
            bin_dir = current / "venv" / "bin"
            return Install(env_file, state_dir, str(bin_dir / "citar"), str(bin_dir / "citar-admin"), current,
                           documentation)
    bin_dir = Path(sys.prefix) / "bin"
    citar = str(bin_dir / "citar") if (bin_dir / "citar").is_file() else shutil.which("citar")
    admin = str(bin_dir / "citar-admin") if (bin_dir / "citar-admin").is_file() else shutil.which("citar-admin")
    return Install(env_file, state_dir, citar or f"{sys.executable} -P -m citar", admin, LAYOUT / "current",
                   documentation)


# --------------------------------------------------------------------------- the files

def read(name: str) -> str:
    """The package's copy of ``deploy/<name>``, with LF line ends whatever the checkout did."""
    return (DEPLOY / name).read_text(encoding="utf-8").replace("\r\n", "\n")


def directives(text: str, section: str) -> list[tuple[str, str]]:
    """A unit file's ``Key=value`` lines in ``[section]``, in order (comments and blank lines left out)."""
    found, current = [], None
    for line in text.split("\n"):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            current = stripped[1:-1]
        elif current == section and stripped and stripped[0] not in "#;" and "=" in stripped:
            key, _, value = stripped.partition("=")
            found.append((key, value))
    return found


def set_directives(text: str, section: str, values: dict[str, str]) -> str:
    """``text`` with each key of ``values`` in ``[section]`` given that value. Each must be there once: a
    renamed or repeated directive in deploy/'s file is an error here, not a unit written without it."""
    lines, current, seen = text.split("\n"), None, set()
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            current = stripped[1:-1]
            continue
        key = stripped.partition("=")[0]
        if current == section and "=" in stripped and stripped[0] not in "#;" and key in values:
            if key in seen:
                raise ValueError(f"{key}= is in [{section}] twice")
            lines[i] = f"{key}={values[key]}"
            seen.add(key)
    missing = sorted(set(values) - seen)
    if missing:
        raise ValueError(f"[{section}] has no {', '.join(k + '=' for k in missing)}")
    return "\n".join(lines)


def set_variables(text: str, values: dict[str, str]) -> str:
    """``text`` (a script) with each ``NAME="..."`` assignment of ``values`` given that value; each must be
    there exactly once, on a line of its own."""
    for name, value in values.items():
        pattern = re.compile(rf'^{re.escape(name)}="[^"\n]*"$', re.M)
        found = pattern.findall(text)
        if len(found) != 1:
            raise ValueError(f'expected one {name}="..." line, found {len(found)}')
        line = f'{name}="{value}"'
        text = pattern.sub(lambda _, line=line: line, text)
    return text


def _checked(install: Install) -> Install:
    problems = install.problems()
    if problems:
        raise ValueError("; ".join(problems))
    return install


def service_unit(install: Install) -> str:
    """citar.service for this server."""
    install = _checked(install)
    state = posix(install.state_dir)
    text = set_directives(read("citar.service"), "Unit", {"Documentation": install.documentation})
    return set_directives(text, "Service", {
        "ExecStart": f"{install.citar} serve",
        "WorkingDirectory": state,
        "Environment": f"CITAR_STATE_DIR={state}",
        "EnvironmentFile": posix(install.env_file),
        "ReadWritePaths": state,
    })


def admin_script(install: Install) -> Optional[str]:
    """citar-admin for this server, or None when this copy has no citar-admin command to run."""
    install = _checked(install)
    if install.citar_admin is None:
        return None
    return set_variables(read("citar-admin.sh"), {
        "ENV_FILE": posix(install.env_file), "STATE_DIR": posix(install.state_dir), "PROGRAM": install.citar_admin})


def backup_script(install: Install) -> str:
    """citar-backup for this server."""
    install = _checked(install)
    return set_variables(read("citar-backup.sh"), {
        "ENV_FILE": posix(install.env_file), "STATE_DIR": posix(install.state_dir), "CURRENT": posix(install.current)})


def rendered(install: Install) -> dict[str, Optional[str]]:
    """Every file for this server, by its name in deploy/ (None for a wrapper this copy cannot have)."""
    return {
        "citar.service": service_unit(install),
        "citar-admin.sh": admin_script(install),
        "citar-backup.sh": backup_script(install),
        "citar-backup.service": read("citar-backup.service"),
        "citar-backup.timer": read("citar-backup.timer"),
    }
