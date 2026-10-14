"""The helper's configuration table (design/phase4-helper.md P4.3.2; package 4-03, gate 1).

The Rust helper must read every protocol 1 way of configuring the helper with the same meaning:
``helper.json``, ``--config worker.json``, the ``CITAR_WORKER_*`` variables and the flags, flag over
file over environment. This module records what the Python worker's ``build_config``
(``citar/worker/__main__.py``) makes of a table of configurations into
``tests/data/helper_config_cases.json``, and holds that file to it while the worker exists;
``crates/citar-helper/tests/config_cases.rs`` holds the Rust helper to the same file. Package 4-05
deletes the Python worker and this module with it; the JSON stays, as the record.

Re-record after a deliberate change of the table with ``python -m tests.test_helper_config_cases
--record``. A case the Rust helper answers differently on purpose carries ``fix`` (a key of
``fixes``, which says why) and ``helper``, the Rust answer.
"""
from __future__ import annotations

import io
import json
import logging
import os
import sys
import tempfile
import unittest
from dataclasses import asdict
from pathlib import Path
from unittest import mock

DATA = Path(__file__).resolve().parent / "data" / "helper_config_cases.json"

#: What the table compares: protocol 1's WorkerConfig, field by field.
FIELDS = ("server_url", "token", "base_url", "provider", "api_key_env", "max_concurrent", "name",
          "quiet_hours", "models", "insecure", "collect_hardware", "hardware_file")

ALL_ENV = {
    "CITAR_WORKER_SERVER": "https://env.example.com",
    "CITAR_WORKER_TOKEN": "cwt_from_env",
    "CITAR_WORKER_BASE_URL": "http://env-host:1234/v1",
    "CITAR_WORKER_PROVIDER": "ollama",
    "CITAR_WORKER_API_KEY_ENV": "ENV_KEY_VAR",
    "CITAR_WORKER_MAX_CONCURRENT": "5",
    "CITAR_WORKER_NAME": "env-name",
    "CITAR_WORKER_HARDWARE_FILE": "/env/hardware.json",
}

FILE = {
    "server_url": "https://file.example.com",
    "token": "cwt_from_file",
    "base_url": "http://file-host:11434/v1",
    "provider": "openai_compatible",
    "max_concurrent": 2,
    "name": "file-name",
}

GIVEN = ["--server", "https://flag.example.com", "--token", "cwt_from_flag"]

#: The inputs. ``{dir}`` is each case's own directory: the home (``CITAR_DATA_DIR``), where
#: ``helper.json`` sits, and where the other files of ``files`` are written.
CASES = [
    {"name": "helper.json alone", "args": [], "files": {"helper.json": {
        "server_url": "https://citar.example.com", "token": "cwt_helperjson", "base_url": "http://localhost:11434/v1",
        "provider": "ollama", "max_concurrent": 2, "name": "Framework Desktop", "quiet_hours": ["22:00-06:00"],
        "models": ["gemma", "qwen"]}}},
    {"name": "a 0.1.5 first-run helper.json", "migrated": True, "args": [], "files": {"helper.json": {
        "server_url": "https://citar.example.com", "token": "cwt_first_run"}}},
    {"name": "worker.json by --config, helper.json ignored", "args": ["--config", "{dir}/worker.json"], "files": {
        "helper.json": {"server_url": "https://ignored.example.com", "token": "cwt_ignored", "name": "ignored"},
        "worker.json": {"server_url": "https://citar.example.com", "token": "cwt_workerjson",
                        "base_url": "http://localhost:1234/v1", "max_concurrent": 1,
                        "quiet_hours": [[0, 0, 360], [1, 0, 360]]}}},
    {"name": "environment only", "args": [], "env": ALL_ENV},
    {"name": "flags over file over environment", "args": GIVEN + ["--max-concurrent", "3"],
     "env": ALL_ENV, "files": {"helper.json": FILE}},
    {"name": "file over environment", "args": [], "env": ALL_ENV, "files": {"helper.json": FILE}},
    {"name": "every protocol 1 flag", "args": GIVEN + [
        "--base-url", "http://gpu-box:8080/v1", "--provider", "openai", "--api-key-env", "MY_API_KEY",
        "--max-concurrent", "4", "--name", "flag-name", "--model", "a", "--model", "b",
        "--quiet", "22:00-06:00", "--quiet", "Sat 00:00-09:00", "--insecure", "--hardware", "{dir}/hw.json"]},
    {"name": "a flag of 0 or empty text is not given", "args": GIVEN + ["--max-concurrent", "0", "--name", ""],
     "env": {"CITAR_WORKER_NAME": "env-name"}, "files": {"helper.json": {"max_concurrent": 2}}},
    {"name": "empty or null file values fall through to the environment", "args": [],
     "env": {"CITAR_WORKER_SERVER": "https://env.example.com", "CITAR_WORKER_TOKEN": "cwt_env",
             "CITAR_WORKER_NAME": "env-name"},
     "files": {"helper.json": {"server_url": "", "token": None, "name": "", "base_url": None}}},
    {"name": "empty variables take the defaults", "args": GIVEN,
     "env": {"CITAR_WORKER_BASE_URL": "", "CITAR_WORKER_PROVIDER": "", "CITAR_WORKER_MAX_CONCURRENT": ""}},
    {"name": "the defaults", "args": GIVEN},
    {"name": "quiet hours across midnight", "args": GIVEN + ["--quiet", "22:00-06:00"]},
    {"name": "quiet hours by weekday", "args": GIVEN + ["--quiet", "Sat 00:00-09:00", "--quiet", "Sunday 23:00-01:30"]},
    {"name": "quiet hours in whole hours", "args": GIVEN + ["--quiet", "8-17"]},
    {"name": "quiet hours all day", "args": GIVEN + ["--quiet", "Wed 22:00-22:00"]},
    {"name": "quiet flags replace the file's", "args": GIVEN + ["--quiet", "03:00-04:00"],
     "files": {"helper.json": {"quiet_hours": ["Mon 01:00-02:00"]}}},
    {"name": "quiet hours from the file as text and numbers", "args": GIVEN,
     "files": {"helper.json": {"quiet_hours": ["Tue 09:00-10:00", [3, 0, 60]]}}},
    {"name": "an unknown weekday", "args": GIVEN + ["--quiet", "Xyz 01:00-02:00"]},
    {"name": "a window without a dash", "args": GIVEN + ["--quiet", "2200"]},
    {"name": "the model list from the file", "args": GIVEN, "files": {"helper.json": {"models": ["m1", "m2", "m3"]}}},
    {"name": "model flags replace the file's", "args": GIVEN + ["--model", "only"],
     "files": {"helper.json": {"models": ["m1", "m2"]}}},
    {"name": "an empty model list is none", "args": GIVEN, "files": {"helper.json": {"models": []}}},
    {"name": "--no-hardware", "args": GIVEN + ["--no-hardware"]},
    {"name": "--hardware over the environment's file", "args": GIVEN + ["--hardware", "{dir}/mine.json"],
     "env": {"CITAR_WORKER_HARDWARE_FILE": "/env/hw.json"}},
    {"name": "the hardware file from the file", "args": GIVEN, "files": {"helper.json": {"hardware_file": "/f/hw.json"}}},
    {"name": "insecure from the file", "args": GIVEN, "files": {"helper.json": {"insecure": True}}},
    {"name": "max_concurrent as text in the environment", "args": GIVEN, "env": {"CITAR_WORKER_MAX_CONCURRENT": "3"}},
    {"name": "max_concurrent as text in the file", "args": GIVEN, "files": {"helper.json": {"max_concurrent": "4"}}},
    {"name": "the API key's variable from the file", "args": GIVEN,
     "files": {"helper.json": {"api_key_env": "OPENROUTER_KEY", "provider": "openai_compatible"}}},
    {"name": "protocol 2's keys are ignored by protocol 1 and change nothing it reads", "args": [],
     "files": {"helper.json": {"server_url": "https://citar.example.com", "token": "cwt_v2",
                               "cpu": {"enabled": True, "threads": 2}, "updates": "notify", "stream": False,
                               "no_power": True}}},
    {"name": "a missing token", "args": ["--server", "https://citar.example.com"]},
    {"name": "a missing server", "args": ["--token", "cwt_only"]},
    {"name": "nothing given", "args": []},
    {"name": "a missing --config file", "args": ["--config", "{dir}/absent.json"]},
    {"name": "a token from the file, the server from a flag", "args": ["--server", "https://flag.example.com"],
     "files": {"helper.json": {"token": "cwt_file_token"}}},
]


def _clean(text: str, d: str) -> str:
    """A path-bearing string as the table keeps it: forward slashes, the case's directory as {dir}."""
    return text.replace("\\", "/").replace(d.replace("\\", "/"), "{dir}")


def _subst(value, d: str):
    if isinstance(value, str):
        return value.replace("{dir}", d.replace("\\", "/"))
    if isinstance(value, list):
        return [_subst(v, d) for v in value]
    if isinstance(value, dict):
        return {k: _subst(v, d) for k, v in value.items()}
    return value


def answer(case: dict) -> dict:
    """What protocol 1's build_config makes of one case: ``expect`` or ``error``."""
    from citar.worker import __main__ as wm

    class Parsed(Exception):
        """Stops main() once argparse has read the command line."""

    with tempfile.TemporaryDirectory(prefix="citar-cfg-") as d:
        for name, content in (case.get("files") or {}).items():
            text = content if isinstance(content, str) else json.dumps(content)
            Path(d, name).write_text(text, encoding="utf-8")
        argv = _subst(case.get("args") or [], d)
        box = {}

        def grab(args):
            box["args"] = args
            raise Parsed

        env = {k: v for k, v in os.environ.items() if not k.startswith("CITAR_WORKER_") and k != "CITAR_DATA_DIR"}
        env.update(case.get("env") or {})
        env["CITAR_DATA_DIR"] = d
        with mock.patch.dict(os.environ, env, clear=True), mock.patch.object(sys, "stdin", io.StringIO("")):
            with mock.patch.object(wm, "build_config", grab), mock.patch.object(logging, "basicConfig"):
                try:
                    wm.main(argv)
                except Parsed:
                    pass
            try:
                cfg = wm.build_config(box["args"])
            except SystemExit as exc:
                return {"error": _clean(str(exc.code), d)}
        got = {k: v for k, v in asdict(cfg).items() if k in FIELDS}
        got["quiet_hours"] = [list(w) for w in got["quiet_hours"]]
        return {"expect": json.loads(_clean(json.dumps(got), d))}


def record() -> dict:
    cases = []
    for case in CASES:
        row = dict(case)
        row.update(answer(case))
        cases.append(row)
    return {
        "format": 1,
        "about": "What protocol 1's build_config (citar/worker/__main__.py) makes of each case: \"expect\", the "
                 "WorkerConfig fields the Rust helper's settings must equal, or \"error\", its SystemExit message. "
                 "{dir} is the case's directory: the home (CITAR_DATA_DIR) holding helper.json, and where the other "
                 "files are. stdin is no terminal, so no first-run question is asked. A case \"migrated\" is a 0.1.5 "
                 "helper.json, which must lend no CPU. A case marked \"fix\" is answered differently by the Rust "
                 "helper on purpose (see \"fixes\"), its \"helper\" being the Rust answer. Recorded by "
                 "python -m tests.test_helper_config_cases --record; 4-05 deletes the Python worker, and this file "
                 "stays the record.",
        "fields": list(FIELDS),
        "fixes": {},
        "cases": cases,
    }


class HelperConfigCases(unittest.TestCase):
    def test_the_recorded_table_is_what_build_config_answers(self):
        table = json.loads(DATA.read_text(encoding="utf-8"))
        names = [c["name"] for c in table["cases"]]
        self.assertEqual(names, [c["name"] for c in CASES], "the table's cases are this module's")
        for recorded, case in zip(table["cases"], CASES):
            with self.subTest(case["name"]):
                for key in ("args", "env", "files", "migrated"):
                    self.assertEqual(recorded.get(key), case.get(key), key)
                got = answer(case)
                self.assertEqual({k: recorded[k] for k in ("expect", "error") if k in recorded}, got)
        self.assertGreaterEqual(len(names), 30)
        kinds = {("error" if "error" in c else "expect") for c in table["cases"]}
        self.assertEqual(kinds, {"error", "expect"})


if __name__ == "__main__":
    if "--record" in sys.argv:
        DATA.write_text(json.dumps(record(), indent=2, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")
        print(f"wrote {DATA} ({len(CASES)} cases)")
    else:
        unittest.main()
