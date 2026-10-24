"""``python -m citar``: the ``citar`` command (:mod:`citar.cli`), for a copy whose scripts are not on PATH."""
from .cli import main

if __name__ == "__main__":
    raise SystemExit(main())
