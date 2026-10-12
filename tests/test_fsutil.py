"""Readers of files another thread replaces (citar/fsutil.py). On Windows, for the moment ``os.replace`` takes, the old
file is pending deletion and opening it fails with PermissionError; the suite's merge run of 3-10 caught a probe run's
``run.json`` read so (``test_pooled_work.Probes``), so a reader tries again, as the writer does."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import secrets
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from citar import fsutil, probes


def refusing(times: int, name: str):
    """A ``Path.read_text`` that refuses the file called *name* its first *times* reads, as Windows does while the
    file is being replaced, and the calls it saw."""
    real = Path.read_text
    seen = []

    def read_text(self, *args, **kwargs):
        if self.name == name:
            seen.append(self)
            if len(seen) <= times:
                raise PermissionError(13, "Permission denied", str(self))
        return real(self, *args, **kwargs)
    return read_text, seen


class ReadText(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar-fsutil-"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)

    def test_a_read_refused_for_a_moment_is_tried_again(self):
        p = self.dir / "a.json"
        fsutil.write_text(p, '{"x": 1}')
        read_text, seen = refusing(2, "a.json")
        with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep") as slept:
            self.assertEqual(fsutil.read_text(p), '{"x": 1}')
        self.assertEqual(len(seen), 3)
        self.assertEqual(slept.call_count, 2)

    def test_a_lasting_refusal_still_raises(self):
        p = self.dir / "a.json"
        fsutil.write_text(p, "{}")
        read_text, seen = refusing(10**6, "a.json")
        with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep"):
            with self.assertRaises(PermissionError):
                fsutil.read_text(p)
        self.assertEqual(len(seen), 40)

    def test_a_missing_file_is_not_waited_for(self):
        with mock.patch.object(fsutil.time, "sleep") as slept:
            with self.assertRaises(FileNotFoundError):
                fsutil.read_text(self.dir / "none.json")
        slept.assert_not_called()


class ProbeFiles(unittest.TestCase):
    """A probe run's file is rewritten by its runner while the page, the queue and the runner's other threads read it;
    a probe's file by its editor while the Probes page lists it."""

    def test_a_run_read_while_its_file_is_replaced(self):
        runner = probes.ProbeRunner.__new__(probes.ProbeRunner)   # its files only: no threads, no queue
        rid = f"fsutil-run-{secrets.token_hex(3)}"
        self.addCleanup(shutil.rmtree, probes.RUNS / rid, ignore_errors=True)
        runner._write({"id": rid, "status": "waiting (machine busy)"})
        for read in (lambda: runner.get_run(rid), lambda: next(r for r in runner.list_runs() if r["id"] == rid)):
            read_text, seen = refusing(1, "run.json")
            with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep"):
                self.assertEqual(read()["status"], "waiting (machine busy)")
            self.assertGreaterEqual(len(seen), 2)

    def test_a_probe_read_while_its_file_is_replaced(self):
        pid = f"fsutil-probe-{secrets.token_hex(3)}"
        path = probes._path(pid)
        path.parent.mkdir(parents=True, exist_ok=True)
        fsutil.write_text(path, json.dumps({"id": pid, "name": "fsutil probe", "cases": []}))
        self.addCleanup(probes.delete_probe, pid)
        for read in (lambda: probes.load_probe(pid), lambda: next(p for p in probes.list_probes() if p["id"] == pid)):
            read_text, seen = refusing(1, path.name)
            with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep"):
                self.assertEqual(read()["id"], pid)
            self.assertGreaterEqual(len(seen), 2)


if __name__ == "__main__":
    unittest.main()
