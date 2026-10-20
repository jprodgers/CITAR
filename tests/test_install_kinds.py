"""The installed copies' fixes (design/phase5-release.md P5.6, package 5-05): the kind of install
(:func:`citar.paths.install_kind`), the command that starts its MCP bridge (:func:`citar.paths.mcp_command`), and
``/api/meta``, which gives that command, the version and the label, and in server mode names no folder of the
server's (it is public).

The job host's command per kind is tests/test_job_host.py's (``Commands``); the lobby's use of ``mcp_command`` is
citar/web/tests/mcpcommand.test.mjs's.
"""
from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from citar import __version__, engine_api, paths

ROOT = Path(__file__).resolve().parent.parent


def has_path(value, *folders) -> bool:
    """Whether a string in ``value`` (a JSON value) names any of ``folders`` (case-folded on Windows)."""
    def strings(v):
        if isinstance(v, str):
            yield v
        elif isinstance(v, dict):
            for item in v.values():
                yield from strings(item)
        elif isinstance(v, (list, tuple)):
            for item in v:
                yield from strings(item)
    return any(os.path.normcase(str(folder)) in os.path.normcase(s) for s in strings(value) for folder in folders)


class Kinds(unittest.TestCase):
    def test_this_suite_runs_from_a_checkout(self):
        self.assertEqual(paths.install_kind(), "checkout")

    def test_a_frozen_program_is_the_bundle(self):
        with mock.patch.object(sys, "frozen", True, create=True):
            self.assertEqual(paths.install_kind(), "frozen")

    def test_a_package_with_no_checkout_beside_it_is_a_wheel(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(paths, "ROOT", Path(tmp)):
            self.assertEqual(paths.install_kind(), "wheel")
            # a checkout read-only still counts as one: the kind is where the code is, not whether it is writable
            (Path(tmp) / "pyproject.toml").write_text("", encoding="utf-8")
            self.assertEqual(paths.install_kind(), "checkout")

    def test_the_bundles_programs_are_beside_the_running_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            play = Path(tmp) / "CITAR" / ("citar-play.exe" if os.name == "nt" else "citar-play")
            with mock.patch.object(sys, "executable", str(play)):
                want = Path(tmp).resolve() / "CITAR" / ("citar.exe" if os.name == "nt" else "citar")
                self.assertEqual(paths.bundle_program(), want)


class McpCommand(unittest.TestCase):
    def test_each_kind_starts_its_own_bridge_on_a_local_server(self):
        with tempfile.TemporaryDirectory() as tmp:
            exe = Path(tmp) / "CITAR" / ("citar.exe" if os.name == "nt" else "citar")
            with mock.patch.object(sys, "executable", str(exe.with_name("citar-play.exe"))):
                self.assertEqual(paths.mcp_command(False, "frozen"), [str(exe.resolve()), "mcp"])
        self.assertEqual(paths.mcp_command(False, "checkout"), [sys.executable, str(paths.ROOT / "citar_mcp.py")])
        self.assertTrue((paths.ROOT / "citar_mcp.py").is_file())

    def test_a_wheel_gives_its_citar_mcp_script_else_its_interpreter_with_P(self):
        with tempfile.TemporaryDirectory() as tmp:
            scripts = Path(tmp)
            name = "citar-mcp" + (".exe" if os.name == "nt" else "")
            with mock.patch("sysconfig.get_path", return_value=str(scripts)), \
                    mock.patch.object(sys, "executable", str(scripts / "python")):
                self.assertEqual(paths.mcp_command(False, "wheel"),
                                 [sys.executable, "-P", "-m", "citar.agents.mcp_server"])
                (scripts / name).write_bytes(b"")
                self.assertEqual(paths.mcp_command(False, "wheel"), [str(scripts / name)])

    def test_server_mode_names_no_folder(self):
        self.assertEqual(paths.mcp_command(True, "wheel"), ["citar-mcp"])
        self.assertEqual(paths.mcp_command(True, "checkout"), ["citar-mcp"])
        with mock.patch.object(sys, "executable", str(Path("C:/Program Files/CITAR/citar-play.exe"))):
            frozen = paths.mcp_command(True, "frozen")
        self.assertEqual(frozen, ["citar.exe" if os.name == "nt" else "citar", "mcp"])
        for kind in paths.INSTALL_KINDS:
            for word in paths.mcp_command(True, kind):
                self.assertNotIn("/", word)
                self.assertNotIn("\\", word)

    def test_an_unknown_kind_is_refused(self):
        with self.assertRaises(ValueError):
            paths.mcp_command(False, "zip")


class Meta(unittest.TestCase):
    URL = "http://127.0.0.1:8765"

    def payload(self, server_mode: bool, kind: str) -> dict:
        from citar.server import app as appmod
        with mock.patch.object(paths, "install_kind", return_value=kind):
            return appmod.meta_payload(server_mode, self.URL)

    def test_the_version_the_label_and_the_mcp_command(self):
        info = engine_api.build_info()
        for server_mode in (False, True):
            for kind in paths.INSTALL_KINDS:
                with self.subTest(server_mode=server_mode, kind=kind):
                    m = self.payload(server_mode, kind)
                    self.assertEqual(set(m), {"version", "label", "mode", "server_url", "mcp_command"})
                    self.assertEqual((m["version"], m["label"]), (__version__, info["label"]))
                    self.assertEqual(m["mode"], "server" if server_mode else "local")
                    self.assertEqual(m["server_url"], self.URL)
                    self.assertEqual(m["mcp_command"], paths.mcp_command(server_mode, kind))

    def test_server_mode_names_no_folder_of_the_server(self):
        folders = [paths.ROOT, paths.PACKAGE, Path(sys.executable).parent, Path(sys.prefix), Path(sys.base_prefix)]
        for kind in paths.INSTALL_KINDS:
            with self.subTest(kind=kind):
                m = self.payload(True, kind)
                self.assertFalse(has_path(m, *folders), m)
                self.assertNotIn("python", m)
                self.assertNotIn("project_root", m)
                self.assertNotIn("mcp_script", m)
        # a local server's own player gets its own command, with its folder
        self.assertTrue(has_path(self.payload(False, "checkout"), paths.ROOT))

    def test_the_route_answers_as_the_mode_says(self):
        from fastapi.testclient import TestClient

        from citar import settings
        from citar.server import app as appmod
        client = TestClient(appmod.app, base_url=self.URL)
        r = client.get("/api/meta")
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(r.json(), appmod.meta_payload(settings.get().server_mode, self.URL))


if __name__ == "__main__":
    unittest.main()
