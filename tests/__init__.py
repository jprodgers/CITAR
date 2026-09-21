"""Test package. Games created by tests are saved to a temporary folder, not the real saves/ directory, and the server
registry is a small temporary one (a host PC, the Anthropic API and the dry-run server), not config/servers.json."""
import atexit
import json
import os
import shutil
import tempfile

if not os.environ.get("CITAR_SAVE_DIR"):
    _tmp = tempfile.mkdtemp(prefix="citar-test-saves-")
    os.environ["CITAR_SAVE_DIR"] = _tmp
    atexit.register(shutil.rmtree, _tmp, ignore_errors=True)

if not os.environ.get("CITAR_CONFIG_DIR"):
    _cfg = tempfile.mkdtemp(prefix="citar-test-config-")
    os.environ["CITAR_CONFIG_DIR"] = _cfg
    os.environ["CITAR_KEYS_FILE"] = os.path.join(_cfg, "keys.enc")
    atexit.register(shutil.rmtree, _cfg, ignore_errors=True)
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
    with open(os.path.join(_cfg, "servers.json"), "w", encoding="utf-8") as f:
        json.dump(TEST_REGISTRY, f)
