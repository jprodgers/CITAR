"""The engine has one door: citar/engine_api.py.

Everything in the package and the scripts reaches the game through the facade's public names (its ``__all__``). This
test reads every module and fails on a way round it: importing the engine's extension (``citar._engine``) or the
module behind the facade (``citar._facade_rust``), by name or at run time; taking a name from the facade that is not in
its ``__all__``; or taking the engine's game out of an EngineGame (its private ``_g``). Only the facade's two modules
import the extension; the tests may.
"""
import ast
import unittest
from pathlib import Path
from typing import Optional

PACKAGE = Path(__file__).resolve().parent.parent / "citar"
SCRIPTS = PACKAGE.parent / "scripts"
FACADE = PACKAGE / "engine_api.py"
#: The facade's modules: the door, and the module behind it that binds the extension.
FACADE_MODULES = {FACADE, PACKAGE / "_facade_rust.py"}
#: What only the facade imports: the extension and the module behind the door.
BEHIND = ("citar._engine", "citar._facade_rust")
#: The EngineGame attribute that holds the engine's game.
GAME_ATTRS = {"_g"}


def _facade_all() -> set:
    """The facade's public surface: its ``__all__``, read without importing it."""
    for node in ast.parse(FACADE.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "__all__" for t in node.targets):
            return set(ast.literal_eval(node.value))
    raise AssertionError("citar/engine_api.py has no __all__")


PUBLIC = _facade_all()


def _behind(module: str) -> bool:
    """Whether importing this module goes round the facade."""
    return any(module == b or module.startswith(b + ".") for b in BEHIND)


def _module_name(path: Path) -> str:
    """The dotted module name of a file in the package, or ``scripts.<name>`` for a script."""
    if PACKAGE not in path.parents:
        return "scripts." + path.stem
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
    text a .format() or % is applied to, up to its first field. None when nothing about it is fixed."""
    if isinstance(node, ast.Constant):
        return node.value if isinstance(node.value, str) else None
    if isinstance(node, ast.JoinedStr):
        first = node.values[0] if node.values else None
        return str(first.value) if isinstance(first, ast.Constant) else None
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _fixed_start(node.left)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Mod):
        start = _fixed_start(node.left)
        return None if start is None else start.split("%", 1)[0]
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "format":
        start = _fixed_start(node.func.value)
        return None if start is None else start.split("{", 1)[0]
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
        # a relative name, resolved against the package argument: import_module("._engine", "citar")
        pkg = node.args[1] if len(node.args) > 1 else next((k.value for k in node.keywords if k.arg == "package"), None)
        if isinstance(pkg, ast.Constant) and isinstance(pkg.value, str):
            level = len(name) - len(name.lstrip("."))
            base = pkg.value.split(".")
            name = ".".join(base[:len(base) - (level - 1)] + [name[level:]])
    return name


def _bad_name(name: str) -> bool:
    """Whether an import_module target goes round the facade (see _import_by_name for the trailing "*")."""
    if name.endswith("*"):
        start = name[:-1]
        return any(b.startswith(start) or start.startswith(b) for b in BEHIND)
    return _behind(name)


def violations(source: str, module_name: str, is_package: bool = False) -> list[str]:
    """Every way this module's source reaches past the facade, as "line: what"."""
    tree = ast.parse(source)
    out = []
    facade = set()            # the local names bound to the facade module: `from .. import engine_api as api`
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            out += [f"{node.lineno}: import {a.name}" for a in node.names if _behind(a.name)]
            facade |= {a.asname for a in node.names if a.name == "citar.engine_api" and a.asname}
        elif isinstance(node, ast.ImportFrom):
            target = _resolve(module_name, is_package, node)
            if _behind(target):
                out.append(f"{node.lineno}: from {target} import ...")
            elif target == "citar":
                # the package whose modules can be imported by name: `from citar import _engine`
                out += [f"{node.lineno}: from citar import {a.name}" for a in node.names if _behind(f"citar.{a.name}")]
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
    def test_nothing_goes_round_the_facade(self):
        found = []
        for path in sorted([*PACKAGE.rglob("*.py"), *SCRIPTS.rglob("*.py")]):
            if path in FACADE_MODULES:
                continue
            for v in violations(path.read_text(encoding="utf-8"), _module_name(path), path.name == "__init__.py"):
                found.append(f"{path.relative_to(PACKAGE.parent).as_posix()}:{v}")
        self.assertEqual(found, [], "these reach the engine without citar.engine_api:\n" + "\n".join(found))

    def test_the_checker_catches_each_way_round(self):
        cases = [
            "from .. import _engine",                      # the extension, relative
            "import citar._engine as E",                   # ... absolute
            "from citar._engine import Game",
            "from .._facade_rust import EngineGame",       # the module behind the door
            "import citar._facade_rust",
            "importlib.import_module('citar._engine')",    # by name at run time
            "importlib.import_module(f'citar._facade_{kind}')",        # ... by an f-string
            "importlib.import_module('citar._' + kind)",               # ... by concatenation
            "importlib.import_module('citar.{}'.format(kind))",        # ... by format
            "importlib.import_module('citar.%s' % kind)",              # ... by %
            "importlib.import_module('._engine', 'citar')",            # ... relative to a package
            "__import__('citar._engine')",                 # the builtin
            "g = session.game._g",                         # the engine's game out of the facade
            "g = getattr(session.game, '_g')",             # ... by name
            "from ..engine_api import Game",               # a name the facade does not hand out
            "from ..engine_api import MAP_DIR, _E",
            "engine_api._E.Game.new(b'{}')",               # the facade's own extension handle
            "engine_api.MAP_DIR",
            "api._loads(b'{}')",                           # the facade under another name
            "citar.engine_api._dumps({})",                 # the facade by its full name
            "getattr(engine_api, '_E')",
        ]
        header = ["import importlib", "import citar.engine_api", "from .. import engine_api",
                  "from .. import engine_api as api"]
        for case in cases:
            with self.subTest(case=case):
                self.assertTrue(violations("\n".join(header + [case]), "citar.server.app"), case)
        found = violations("\n".join(header + cases), "citar.server.app")
        # two violations on the MAP_DIR, _E line
        self.assertEqual(len(found), len(cases) + 1, found)

    def test_the_facade_and_bookkeeping_are_allowed(self):
        src = "\n".join(["from .. import engine_api", "from ..engine_api import ActionError, EngineGame",
                         "from ..bots import profiles, ratings", "from ..bots.ratings import best_profile",
                         "import citar.engine_api", "from .. import engine_api as api",
                         "engine_api.run_game(spec)", "api.bot_instance('basic')", "citar.engine_api.tool_list()",
                         "importlib.import_module(name)", "importlib.import_module('citar.bots.profiles')",
                         "importlib.import_module(f'citar.pool.{kind}')", "session.game.execute(0, 'end_turn')",
                         "getattr(engine_api, 'map_sizes')()", "x = engine_api.build_info()",
                         "engine_api.check_ruleset(path)"])
        self.assertEqual(violations(src, "citar.server.session"), [])

    def test_all_is_the_whole_public_surface(self):
        # The door defines nothing of its own: every name is the module behind it's, which must have exactly the
        # names of the door's __all__.
        import importlib
        from citar import engine_api
        behind = importlib.import_module("citar._facade_rust")
        self.assertEqual(set(behind.__all__), PUBLIC, "citar/_facade_rust.py's __all__ is not citar/engine_api.py's")
        self.assertEqual({n for n in PUBLIC if not hasattr(engine_api, n)}, set(), "in __all__ but not defined")
        self.assertFalse(PUBLIC & {"Game", "GameState", "get_rules", "RULES_VERSION", "BACKEND", "BackendError"})
        self.assertTrue(issubclass(engine_api.EngineCrash, RuntimeError))
        self.assertFalse(issubclass(engine_api.EngineCrash, engine_api.ActionError))
        tree = ast.parse(FACADE.read_text(encoding="utf-8"))
        defined = {n.name for n in tree.body if isinstance(n, (ast.FunctionDef, ast.ClassDef))}
        self.assertEqual(defined, set(), "citar/engine_api.py defines nothing of its own")


if __name__ == "__main__":
    unittest.main()
