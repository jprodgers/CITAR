"""Console entry point for the bundled Windows build.

``citar.cli:main`` is the console script an ordinary pip install creates. PyInstaller needs a real
script file to analyse rather than an entry-point name, so this is that file and nothing more: the
bundled ``citar.exe`` behaves exactly like the installed one.
"""
import sys

from citar.cli import main

if __name__ == "__main__":
    sys.exit(main())
