"""Entry point for the standalone CITAR helper (the worker, as one self-contained executable).

The helper is what a person runs on the machine with the GPU: it connects out to a CITAR server and
serves that machine's models to it. The release workflow builds it for Windows, macOS and Linux with
PyInstaller, under names that do not change between versions, so a download link can always point at
``releases/latest/download/<name>``.

    python -m PyInstaller --onefile --name citar-helper-windows-x64 installer/helper_entry.py ...
"""
import sys

from citar.worker.__main__ import main

if __name__ == "__main__":
    sys.exit(main())
