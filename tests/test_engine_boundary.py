"""The engine has one door: citar/engine_api.py.

Everything outside citar/engine and citar/bots reaches the game through the facade, so the Rust engine can replace the
Python one by changing the facade's backend and nothing else. This test reads every module in the package and fails on
a way round it: importing the engine or a bot module, importing one by name at run time, taking a name from the facade
that is not in its ``__all__`` (the engine classes it imports for itself), or taking the Python game out of an
EngineGame (python_game, which is for tests and engine-side tools, or the private _g behind it).
"""
import ast
import unittest
from pathlib import Path
from typing import Optional

PACKAGE = Path(__file__).resolve().parent.parent / "citar"
FACADE = PACKAGE / "engine_api.py"
ENGINE_SIDE = (PACKAGE / "engine", PACKAGE / "bots")
# bot profiles and ratings are bookkeeping about bots (names, parameters, results), not bots: anyone may use them
PYTHON_SIDE_BOTS = {"citar.bots", "citar.bots.profiles", "citar.bots.ratings"}
# the EngineGame attributes that hold the Python engine's live Game
GAME_ATTRS = {"python_game", "_g"}


def _facade_all() -> set:
    """The facade's public surface: its ``__all__``, read without importing it."""
    for node in ast.parse(FACADE.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "__all__" for t in node.targets):
            return set(ast.literal_eval(node.value))
    raise AssertionError("citar/engine_api.py has no __all__")


PUBLIC = _facade_all()


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


def _fixed_start(node: ast.AST) -> Optional[str]:
    """The part of a string expression known before it runs: an f-string's or a concatenation's leading text, or the
    text a .format() or % is applied to. None when nothing about it is fixed."""
    if isinstance(node, ast.Constant):
        return node.value if isinstance(node.value, str) else None
    if isinstance(node, ast.JoinedStr):
        first = node.values[0] if node.values else None
        return str(first.value) if isinstance(first, ast.Constant) else None
    if isinstance(node, ast.BinOp) and isinstance(node.op, (ast.Add, ast.Mod)):
        return _fixed_start(node.left)
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "format":
        return _fixed_start(node.func.value)
    return None


def _dotted(node: ast.AST) -> str:
    """``a.b.c`` for a chain of attribute reads on a name; "" for anything else."""
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        head = _dotted(node.value)
        return f"{head}.{node.attr}" if head else ""
    return ""


def _import_by_name(node: ast.Call) -> Optional[str]:
    """What an import_module/__import__ call is judged on: the module name, whole when it is a literal, else its fixed
    start with "*" after it (the rest is chosen at run time, so it may be any module under that start)."""
    arg = node.args[0]
    if isinstance(arg, ast.Constant):
        name = arg.value if isinstance(arg.value, str) else None
    else:
        start = _fixed_start(arg)
        name = None if start is None else start + "*"
    if name and name.startswith("."):
        # a relative name, resolved against the package argument: import_module(".basic", "citar.bots")
        pkg = node.args[1] if len(node.args) > 1 else next((k.value for k in node.keywords if k.arg == "package"), None)
        if isinstance(pkg, ast.Constant) and isinstance(pkg.value, str):
            level = len(name) - len(name.lstrip("."))
            base = pkg.value.split(".")
            name = ".".join(base[:len(base) - (level - 1)] + [name[level:]])
    return name


def _bad_name(name: str) -> bool:
    """Whether an import_module target goes round the facade (see _import_by_name for the trailing "*")."""
    if name.endswith("*"):
        return name[:-1].startswith(("citar.engine", "citar.bots."))
    return _forbidden(name)


def violations(source: str, module_name: str, is_package: bool = False) -> list[str]:
    """Every way this module's source reaches past the facade, as "line: what"."""
    tree = ast.parse(source)
    out = []
    facade = set()            # the local names bound to the facade module: `from .. import engine_api as api`
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            out += [f"{node.lineno}: import {a.name}" for a in node.names if _forbidden(a.name)]
            facade |= {a.asname for a in node.names if a.name == "citar.engine_api" and a.asname}
        elif isinstance(node, ast.ImportFrom):
            target = _resolve(module_name, is_package, node)
            if _forbidden(target):
                out.append(f"{node.lineno}: from {target} import ...")
            elif target in ("citar", "citar.bots"):
                # the packages whose modules can be imported by name: `from citar import engine`, `from ..bots import basic`
                out += [f"{node.lineno}: from {target} import {a.name}" for a in node.names
                        if _forbidden(f"{target}.{a.name}")]
                if target == "citar":
                    facade |= {a.asname or a.name for a in node.names if a.name == "engine_api"}
            elif target == "citar.engine_api":
                out += [f"{node.lineno}: from citar.engine_api import {a.name} (not in its __all__)"
                        for a in node.names if a.name != "*" and a.name not in PUBLIC]
    for node in ast.walk(tree):
        if isinstance(node, ast.Attribute):
            if node.attr in GAME_ATTRS:
                out.append(f"{node.lineno}: .{node.attr}")
            elif (_dotted(node.value) in facade or _dotted(node.value) == "citar.engine_api") \
                    and node.attr not in PUBLIC:
                out.append(f"{node.lineno}: engine_api.{node.attr} (not in its __all__)")
        elif isinstance(node, ast.Call):
            fn = node.func
            name = fn.attr if isinstance(fn, ast.Attribute) else fn.id if isinstance(fn, ast.Name) else ""
            if name in ("import_module", "__import__") and node.args:
                target = _import_by_name(node)
                if target and _bad_name(target):
                    out.append(f"{node.lineno}: {name}({ast.unparse(node.args[0])})")
            elif name in ("getattr", "hasattr", "setattr") and len(node.args) >= 2 \
                    and isinstance(node.args[1], ast.Constant) and isinstance(node.args[1].value, str):
                attr = node.args[1].value
                if attr in GAME_ATTRS or (_dotted(node.args[0]) in facade and attr not in PUBLIC):
                    out.append(f"{node.lineno}: {name}(..., {attr!r})")
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
        cases = [
            "from ..engine import tools",                  # relative, the package
            "from ..engine.game import Game",              # relative, a module
            "from .. import engine",                       # the package by name
            "import citar.engine.views",                   # absolute
            "from citar.bots.basic import BasicBot",       # a bot
            "from ..bots import headless, profiles",       # a bot module beside an allowed one
            "importlib.import_module(f'citar.bots.{kind}')",           # a bot by name at run time
            "importlib.import_module('citar.bots.' + kind)",           # ... by concatenation
            "importlib.import_module('citar.bots.{}'.format(kind))",   # ... by format
            "importlib.import_module('citar.bots.%s' % kind)",         # ... by %
            "importlib.import_module('.basic', 'citar.bots')",         # ... relative to a package
            "__import__('citar.engine.tools')",            # the builtin
            "g = session.game.python_game",                # the Python game out of the facade
            "g = session.game._g",                         # ... under its private name
            "g = getattr(session.game, '_g')",             # ... by name
            "from ..engine_api import Game",               # an engine class the facade imports for itself
            "from ..engine_api import GameState, get_rules",
            "engine_api._tools.execute(g, 0, 'end_turn')", # the facade's own engine modules
            "engine_api.Game.new({})",
            "api._diplomacy.get_negotiation(g, 1)",        # the facade under another name
            "citar.engine_api.RULES_VERSION",              # the facade by its full name
            "getattr(engine_api, 'get_rules')()",
        ]
        header = ["import importlib", "import citar.engine_api", "from .. import engine_api",
                  "from .. import engine_api as api"]
        for case in cases:
            with self.subTest(case=case):
                self.assertTrue(violations("\n".join(header + [case]), "citar.server.app"), case)
        found = violations("\n".join(header + cases), "citar.server.app")
        self.assertFalse(any("profiles" in f for f in found), found)
        # two violations on the GameState, get_rules line
        self.assertEqual(len(found), len(cases) + 1, found)

    def test_python_side_modules_are_allowed(self):
        src = "\n".join(["from .. import engine_api", "from ..engine_api import ActionError, EngineGame",
                         "from ..bots import profiles, ratings", "from ..bots.ratings import best_profile",
                         "import citar.engine_api", "from .. import engine_api as api",
                         "engine_api.run_game(spec)", "api.bot_instance('basic')", "citar.engine_api.tool_list()",
                         "importlib.import_module(name)", "importlib.import_module('citar.bots.profiles')",
                         "importlib.import_module(f'citar.pool.{kind}')", "session.game.execute(0, 'end_turn')",
                         "getattr(engine_api, 'map_sizes')()"])
        self.assertEqual(violations(src, "citar.server.session"), [])

    def test_all_is_the_whole_public_surface(self):
        # a public function or constant left out of __all__ would read as backend to the checker above
        tree = ast.parse(FACADE.read_text(encoding="utf-8"))
        defined = set()
        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
                defined.add(node.name)
            elif isinstance(node, ast.Assign):
                defined |= {t.id for t in node.targets if isinstance(t, ast.Name)}
            elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                defined.add(node.target.id)
        defined = {n for n in defined if not n.startswith("_")}
        self.assertEqual(defined - PUBLIC, set(), "public in citar/engine_api.py but missing from its __all__")
        from citar import engine_api
        self.assertEqual({n for n in PUBLIC if not hasattr(engine_api, n)}, set(), "in __all__ but not defined")
        self.assertFalse(PUBLIC & {"Game", "GameState", "get_rules", "RULES_VERSION"})


if __name__ == "__main__":
    unittest.main()
