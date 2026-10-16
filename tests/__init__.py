"""Test package. Games created by tests are saved to a temporary folder, not the real saves/ directory, the accounts
database is a temporary one, never the per-user citar.db, and its machines are the seeded test registry: a host PC, a
dry-run machine, a dry-run "GPU box" and the Anthropic API (``TEST_REGISTRY``), built as rows under those ids through
the registry's own import (``citar.pool.registry_import``, design/phase3-features.md P3.2.8) in every database a test
makes with ``db.create_all()``, and in the default one modules that open none read."""
import atexit
import os
import shutil
import tempfile

# The suite plays the ruleset compiled into the engine, whatever the shell says: a modded one (docs/MODDING.md) would
# change what the tests read. The tests of the variable itself set it in children of their own.
os.environ.pop("CITAR_RULESET_DIR", None)

# Test mode for the prompt scheduler (citar/pool/scheduler.py, design/phase3-features.md P3.3.9): a model call that
# would wait for its machine with a game session's lock held raises instead, wherever in the suite it happens.
os.environ.setdefault("CITAR_TEST_LOCKS", "1")

# The suite's apps start no job dispatcher (citar/jobs/dispatcher.py, design/phase4-helper.md P4.5.5): every test
# module's app shares the process's saves folder, where the lab's queue may hold another test's experiment, and a
# dispatcher would play it on a job host of the host machine's figures. The dispatcher's own tests start theirs, with
# small limits (tests/test_dispatcher.py).
os.environ.setdefault("CITAR_TEST_NO_DISPATCHER", "1")

# The API test modules each point CITAR_DATA_DIR and CITAR_DB_URL at their own temporary database. This default is
# for a module run on its own that builds the app without doing so: without it, the app opens the per-user database.
if not os.environ.get("CITAR_DATA_DIR"):
    _data = tempfile.mkdtemp(prefix="citar-test-data-")
    os.environ["CITAR_DATA_DIR"] = _data
    atexit.register(shutil.rmtree, _data, ignore_errors=True)
_OWN_DB = not os.environ.get("CITAR_DB_URL")
if _OWN_DB:
    os.environ["CITAR_DB_URL"] = "sqlite:///" + os.path.join(os.environ["CITAR_DATA_DIR"], "test.db").replace("\\", "/")

if not os.environ.get("CITAR_SAVE_DIR"):
    _tmp = tempfile.mkdtemp(prefix="citar-test-saves-")
    os.environ["CITAR_SAVE_DIR"] = _tmp
    atexit.register(shutil.rmtree, _tmp, ignore_errors=True)

if not os.environ.get("CITAR_CONFIG_DIR"):
    _cfg = tempfile.mkdtemp(prefix="citar-test-config-")
    os.environ["CITAR_CONFIG_DIR"] = _cfg
    os.environ["CITAR_KEYS_FILE"] = os.path.join(_cfg, "keys.enc")
    atexit.register(shutil.rmtree, _cfg, ignore_errors=True)

#: The suite's machines, as the registry file once held them (``tests/test_registry_merge.TestSeed`` checks the rows
#: against it). No file is written: ``seed_registry`` imports the dict.
TEST_REGISTRY = {
    "format": "citar-servers", "version": 1, "currency": "USD", "currency_symbol": "$", "host_server_id": "sv_host",
    "electricity_plans": [{"id": "ep_home", "name": "Home", "periods": [
        {"from": "2000-01-01", "type": "flat", "rate_kwh": 0.20, "fixed_monthly": 10.0, "fee_allocation": "household_kwh",
         "household_kwh_month": 500}]}],
    "servers": [
        {"id": "sv_host", "name": "Test host", "kind": "owned", "connection": {"provider": "none"},
         "hardware": {"source": "manual", "cpu": {"threads": 8}},
         "power": {"idle_w": 20, "cpu_max_w": 40, "gpu_max_w": 0},
         "components": [{"name": "PC", "price": 876.6, "purchased": "2020-01-01", "lifespan_years": 10}],
         "costs": [{"from": "2000-01-01", "electricity_plan_id": "ep_home"}], "models": []},
        {"id": "sv_dryrun", "name": "Dry run", "kind": "test", "connection": {"provider": "dryrun", "max_parallel": 4},
         "models": [{"id": "m_dry", "key": "dry-run"}]},
        {"id": "sv_gpu", "name": "Test GPU box", "kind": "owned",
         "connection": {"provider": "dryrun", "max_parallel": 1},
         "power": {"idle_w": 50, "cpu_max_w": 100, "gpu_max_w": 300, "sampling": "off"},
         "components": [{"name": "Box", "price": 87.66, "purchased": "2020-01-01", "lifespan_years": 10}],
         "costs": [{"from": "2000-01-01", "electricity_plan_id": "ep_home", "fixed_monthly": 0}],
         "models": [{"id": "m_gpu", "key": "dry-run"}]},
        {"id": "sv_api", "name": "Test API", "kind": "api", "connection": {"provider": "anthropic", "key": {"backend": "env", "env": "CITAR_TEST_NO_KEY"}},
         "models": [{"id": "m_opus", "key": "claude-opus-5"}]},
    ]}


def seed_registry() -> dict:
    """Build ``TEST_REGISTRY``'s machines as rows of the current database, through the registry's import
    (``registry_import.run_once``, the path a server's first administrator takes), owned by nobody (administrators
    manage them, as they managed the file). Once per database: the import's marker says it was done."""
    import copy
    from citar import db
    from citar.pool import registry_import
    with db.session() as s:
        return registry_import.run_once(s, None, data=copy.deepcopy(TEST_REGISTRY), source="tests",
                                        audit_row=False) or {}


def share_with_members(*server_ids: str) -> str:
    """Put some of the site's machines in a group of their own (nobody's: administrators manage it) granted to every
    member, as an administrator who lets members use the dry run does: since the registry merge the site's machines
    are rows like any other, and a member reaches one only through a grant (P3.2.8). Returns the group's id."""
    from citar import db
    from citar.db.models import AccessGrant, Server, ServerGroup
    with db.session() as s:
        group = ServerGroup(owner_id=None, name="The site's shared machines", visibility="server")
        s.add(group)
        s.flush()
        s.add(AccessGrant(group_id=group.id, subject_type="everyone"))
        for sid in server_ids:
            s.get(Server, sid).group_id = group.id
        return group.id


def _seeded_create_all(original):
    """``db.create_all`` that seeds the test registry in the database it made: every module that makes its tables
    so gets the suite's machines."""
    def create_all():
        original()
        seed_registry()
    create_all.original = original
    return create_all


def _install_seed():
    """Seed every database the suite makes with ``create_all``, and make the default one now when this package chose
    it (a temporary file; a database the environment names is never touched here), for modules that open no database
    of their own: ``citar.servers`` reads it then, as a command-line tool reads the server's (and the process's own
    database stays unopened, as it was)."""
    from citar import db
    if not getattr(db.create_all, "original", None):
        db.create_all = _seeded_create_all(db.create_all)
    if _OWN_DB:
        db.configure(os.environ["CITAR_DB_URL"])
        db.create_all()
        db.dispose()


_install_seed()


def has_test_ops() -> bool:
    """Whether the engine's build has the test operations (rule scripts, ``EngineGame.inspect`` and ``test_ops``):
    ``cargo xtask develop`` and CI's test builds do, a release wheel does not."""
    from citar import _engine
    return bool(_engine.HAS_TEST_OPS)
