"""Terminal prompts for the setup wizard.

Deliberately small and dependency-free: CITAR is often set up over SSH on a box with nothing
installed, and a setup wizard that needs a TUI library to ask "which port?" is a setup wizard that
fails at the moment it is most needed.

Three things every prompt here gets right, which hand-rolled ``input()`` calls usually do not:

* **A default you can accept with Return**, shown in the prompt, so the whole wizard can be walked
  through by pressing Return when the defaults are right.
* **Ctrl-C and EOF end the wizard cleanly**, with a message saying nothing was changed, rather than
  a traceback. ``citar setup`` piped from a script hits EOF on the first question; that must not
  look like a crash.
* **Non-interactive mode**, where a question with no answer supplied is an error naming the flag
  that would have supplied it, instead of a hang waiting on a terminal nobody is watching.
"""
from __future__ import annotations

import sys
from typing import Callable, Optional, Sequence

#: Set by ``citar setup --non-interactive``. Questions then refuse rather than block.
NON_INTERACTIVE = False


class Cancelled(Exception):
    """The operator ended the wizard. Nothing has been written."""


class NeedsAnswer(Exception):
    """A non-interactive run reached a question it had no answer for."""

    def __init__(self, question: str, flag: str = "") -> None:
        super().__init__(
            f"{question}\nThis is a non-interactive run, so there is nobody to ask."
            + (f" Pass {flag}." if flag else ""))


def say(text: str = "") -> None:
    """Print a line of wizard output."""
    print(text)


def heading(text: str) -> None:
    """A section heading, so a long wizard reads as steps rather than a wall of questions."""
    print(f"\n{text}\n{'=' * len(text)}")


def note(text: str) -> None:
    """An indented explanation under a question."""
    for line in text.splitlines():
        print(f"    {line}")


def _read(prompt: str) -> str:
    """Read a line, turning Ctrl-C and end of input into a clean cancellation."""
    try:
        return input(prompt)
    except (EOFError, KeyboardInterrupt):
        print()
        raise Cancelled("Setup cancelled. Nothing was changed.") from None


def ask(question: str, default: str = "", flag: str = "",
        validate: Optional[Callable[[str], Optional[str]]] = None) -> str:
    """Ask for a line of text.

    *validate* returns an error message for a bad answer, or ``None`` when it is acceptable; the
    question is asked again until it passes. That keeps validation next to the question rather than
    in a second pass at the end, where the operator has forgotten what they typed.
    """
    if NON_INTERACTIVE:
        if default:
            return default
        raise NeedsAnswer(question, flag)
    suffix = f" [{default}]" if default else ""
    while True:
        answer = _read(f"{question}{suffix}: ").strip() or default
        if validate:
            problem = validate(answer)
            if problem:
                note(problem)
                continue
        return answer


def ask_yes_no(question: str, default: bool = True, flag: str = "") -> bool:
    """Ask a yes/no question. Returns the default on a bare Return."""
    if NON_INTERACTIVE:
        return default
    suffix = " [Y/n]" if default else " [y/N]"
    while True:
        answer = _read(f"{question}{suffix}: ").strip().lower()
        if not answer:
            return default
        if answer in ("y", "yes"):
            return True
        if answer in ("n", "no"):
            return False
        note("Please answer y or n.")


def ask_choice(question: str, options: Sequence[tuple[str, str, str]], default: str = "",
               flag: str = "") -> str:
    """Ask for one of several options.

    Each option is ``(key, title, explanation)``. The explanation is printed under the title because
    the whole reason a wizard beats a config file is that it can say what a choice means at the
    moment the choice is made.
    """
    if NON_INTERACTIVE:
        if default:
            return default
        raise NeedsAnswer(question, flag)
    keys = [key for key, _, _ in options]
    print()
    print(question)
    print()
    for index, (key, title, explanation) in enumerate(options, start=1):
        marker = " (default)" if key == default else ""
        print(f"  {index}. {title}{marker}")
        for line in explanation.splitlines():
            print(f"     {line}")
        print()
    while True:
        answer = _read(f"Choose 1-{len(options)}" + (f" [{keys.index(default) + 1}]" if default else "") + ": ").strip()
        if not answer and default:
            return default
        if answer.isdigit() and 1 <= int(answer) <= len(options):
            return keys[int(answer) - 1]
        if answer.lower() in keys:
            return answer.lower()
        note(f"Please enter a number from 1 to {len(options)}.")


def ask_secret(question: str, flag: str = "", allow_empty: bool = False) -> str:
    """Ask for something that must not be echoed or stored in shell history.

    Falls back to a visible prompt only when there is no terminal to hide the typing — and says so,
    because somebody pasting an API key deserves to know it is on screen.
    """
    if NON_INTERACTIVE:
        if allow_empty:
            return ""
        raise NeedsAnswer(question, flag)
    import getpass

    while True:
        try:
            answer = getpass.getpass(f"{question} (not shown): ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            raise Cancelled("Setup cancelled. Nothing was changed.") from None
        except Exception:
            note("This terminal cannot hide input, so what you type will be visible.")
            answer = _read(f"{question}: ").strip()
        if answer or allow_empty:
            return answer
        note("That cannot be empty.")


def ask_port(question: str, default: int, flag: str = "") -> int:
    """Ask for a TCP port, and reject the ones that will not work."""

    def check(value: str) -> Optional[str]:
        """Validate a port number, rejecting the ones that will not work."""
        if not value.isdigit():
            return "A port is a number, such as 8765."
        port = int(value)
        if not 1 <= port <= 65535:
            return "Ports run from 1 to 65535."
        if port < 1024 and sys.platform != "win32":
            return "Ports below 1024 need root. Use something above 1024 and put a proxy in front."
        return None

    return int(ask(question, str(default), flag, validate=check))


def confirm_plan(title: str, lines: Sequence[str], default: bool = True) -> bool:
    """Show everything that is about to happen, and ask once.

    This is the only place the wizard asks for permission to change the machine, so it lists
    absolutely everything it will write — including files it will create outside the project.
    """
    print()
    print(title)
    print("-" * len(title))
    for line in lines:
        print(f"  {line}")
    print()
    return ask_yes_no("Go ahead?", default)
