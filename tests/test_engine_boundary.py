"""The engine has one door: citar/engine_api.py.

Everything outside citar/engine and citar/bots reaches the game through the facade, so the Rust engine can replace the
Python one by changing the facade's backend and nothing else. This test reads every module in the package and fails on
a way round it: importing the engine or a bot module, importing one by name at run time, or taking the Python game out
of the facade (EngineGame.python_game, which is for tests and engine-side tools).
"""
import ast
import unittest
from pathlib import Path

PACKAGE = Path(__file__).resolve().parent.parent / "citar"
FACADE = PACKAGE / "engine_api.py"
ENGINE_SIDE = (PACKAGE / "engine", PACKAGE / "bots")
# bot profiles and ratings are bookkeeping about bots (names, parameters, results), not bots: anyone may use them
PYTHON_SIDE_BOTS = {"citar.bots", "citar.bots.profiles", "citar.bots.ratings"}


def _forbidden(module: str) -> bool:
    """Whether importing this module goes round the facade."""
    if module == "citar.engine" or module.startswith("citar.engine."):
        return True
    return module.startswith("citar.bots.") and module not in PYTHON_SIDE_BOTS


def _module_name(path: Path) -> str:
    """The dotted module name of a file in the package."""
    rel = path.relative_to(PACKAGE.parent).with_suffix("")
    parts = list(rel.parts)
    if parts[-1] == "__init__":
        parts.pop()
    return ".".join(parts)


def _resolve(module_name: str, is_package: bool, node: ast.ImportFrom) -> str:
    """The absolute module a (possibly relative) from-import names."""
    if not node.level:
        return node.module or ""
    base = module_name.split(".")
    if not is_package:
        base = base[:-1]
    base = base[:len(base) - (node.level - 1)]
    return ".".join(base + ([node.module] if node.module else []))


def violations(source: str, module_name: str, is_package: bool = False) -> list[str]:
    """Every way this module's source reaches past the facade, as "line: what"."""
    out = []
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Import):
            out += [f"{node.lineno}: import {a.name}" for a in node.names if _forbidden(a.name)]
        elif isinstance(node, ast.ImportFrom):
            target = _resolve(module_name, is_package, node)
            if _forbidden(target):
                out.append(f"{node.lineno}: from {target} import ...")
            elif target in ("citar", "citar.bots"):
                # the packages whose modules can be imported by name: `from citar import engine`, `from ..bots import basic`
                out += [f"{node.lineno}: from {target} import {a.name}" for a in node.names
                        if _forbidden(f"{target}.{a.name}")]
        elif isinstance(node, ast.Call):
            fn = node.func
            name = fn.attr if isinstance(fn, ast.Attribute) else fn.id if isinstance(fn, ast.Name) else ""
            if name in ("import_module", "__import__") and node.args:
                arg = node.args[0]
                if isinstance(arg, ast.Constant) and isinstance(arg.value, str):
                    bad = _forbidden(arg.value)
                elif isinstance(arg, ast.JoinedStr) and arg.values and isinstance(arg.values[0], ast.Constant):
                    # a name built at run time, judged by its fixed start: f"citar.bots.{kind}" may be any bot
                    bad = str(arg.values[0].value).startswith(("citar.engine", "citar.bots."))
                else:
                    bad = False
                if bad:
                    out.append(f"{node.lineno}: {name}({ast.unparse(arg)})")
        elif isinstance(node, ast.Attribute) and node.attr == "python_game":
            out.append(f"{node.lineno}: .python_game")
    return out


class EngineBoundaryTests(unittest.TestCase):
    def test_nothing_outside_the_engine_goes_round_the_facade(self):
        found = []
        for path in sorted(PACKAGE.rglob("*.py")):
            if path == FACADE or any(side in path.parents for side in ENGINE_SIDE):
                continue
            for v in violations(path.read_text(encoding="utf-8"), _module_name(path), path.name == "__init__.py"):
                found.append(f"{path.relative_to(PACKAGE.parent).as_posix()}:{v}")
        self.assertEqual(found, [], "these reach the engine without citar.engine_api:\n" + "\n".join(found))

    def test_the_checker_catches_each_way_round(self):
        src = "\n".join([
            "from ..engine import tools",                  # relative, the package
            "from ..engine.game import Game",              # relative, a module
            "from .. import engine",                       # the package by name
            "import citar.engine.views",                   # absolute
            "from citar.bots.basic import BasicBot",       # a bot
            "from ..bots import headless, profiles",       # a bot module beside an allowed one
            "import importlib",
            "importlib.import_module(f'citar.bots.{kind}')",   # a bot by name at run time
            "g = session.game.python_game",                # the Python game out of the facade
        ])
        found = violations(src, "citar.server.app")
        self.assertEqual(len(found), 8, found)
        self.assertFalse(any("profiles" in f for f in found))

    def test_python_side_modules_are_allowed(self):
        src = "\n".join(["from .. import engine_api", "from ..engine_api import ActionError",
                         "from ..bots import profiles, ratings", "from ..bots.ratings import best_profile",
                         "import citar.engine_api"])
        self.assertEqual(violations(src, "citar.server.session"), [])


if __name__ == "__main__":
    unittest.main()
