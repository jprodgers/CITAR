"""File helpers that survive Windows file locks. Replacing or deleting a file fails while another process or thread
has it open (a web request reading it, OneDrive syncing the saves folder); those locks last milliseconds, so retry."""
from __future__ import annotations

import os
import time
from pathlib import Path


def retry(fn, tries: int = 40, delay: float = 0.1):
    """Call *fn*, retrying while Windows says the file is locked.

    The locks this works around last milliseconds - another thread reading a save, or a sync
    client touching the folder - so a short retry loop beats every alternative.
    """
    for k in range(tries):
        try:
            return fn()
        except PermissionError:
            if k == tries - 1:
                raise
            time.sleep(delay)


def replace(src, dst):
    """Replace a file, retrying while it is locked."""
    retry(lambda: os.replace(src, dst))


def write_text(path, text: str):
    """Atomic write: a temporary file next to the target, then a (retried) replace."""
    path = Path(path)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(text, encoding="utf-8")
    replace(tmp, path)
