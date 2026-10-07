"""Protocol source refresh refuses producer drift and preserves authored files."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "protocol_snapshots", Path(__file__).parents[1] / "protocol-snapshots.py")
SNAPSHOTS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SNAPSHOTS)


class ProtocolSnapshots(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.path = Path("crate/src/generated/resource_scopes.rs")
        self.contents = SNAPSHOTS.MARKER + b"\ncurrent scopes\n"
        (self.root / "build/protocol").mkdir(parents=True)
        (self.root / "build/protocol/outputs.txt").write_text(str(self.path) + "\n")
        self.directory = self.root / self.path.parent
        self.directory.mkdir(parents=True)
        (self.directory / "forks.rs").write_bytes(SNAPSHOTS.MARKER + b"\nobsolete forks\n")
        (self.directory / "authored.rs").write_text("// authored helper\n")

    def generate(self, command, check):
        output = Path(command[command.index("--output-root") + 1]) / self.path
        output.parent.mkdir(parents=True)
        output.write_bytes(self.contents)

    def test_check_then_refresh_removes_only_obsolete_producer_outputs(self):
        with patch.object(SNAPSHOTS.subprocess, "check_output", return_value=str(self.path) + "\n"), \
             patch.object(SNAPSHOTS.subprocess, "run", side_effect=self.generate):
            changed, stale = SNAPSHOTS.reconcile(self.root, Path("producer"))
            self.assertEqual(changed, [self.path])
            self.assertEqual(stale, [self.path.parent / "forks.rs"])
            self.assertFalse((self.root / self.path).exists())
            self.assertTrue((self.directory / "forks.rs").exists())
            SNAPSHOTS.reconcile(self.root, Path("producer"), refresh=True)
            self.assertEqual((self.root / self.path).read_bytes(), self.contents)
            self.assertFalse((self.directory / "forks.rs").exists())
            self.assertEqual((self.directory / "authored.rs").read_text(), "// authored helper\n")
            self.assertEqual(SNAPSHOTS.reconcile(self.root, Path("producer")), ([], []))

    def test_roster_drift_refuses_before_generation_or_source_mutation(self):
        with patch.object(SNAPSHOTS.subprocess, "check_output", return_value="wrong.rs\n"), \
             patch.object(SNAPSHOTS.subprocess, "run") as generate:
            with self.assertRaisesRegex(ValueError, "roster differs"):
                SNAPSHOTS.reconcile(self.root, Path("producer"), refresh=True)
            generate.assert_not_called()
            self.assertTrue((self.directory / "forks.rs").exists())

    def test_incomplete_generation_refuses_before_source_mutation(self):
        with patch.object(SNAPSHOTS.subprocess, "check_output", return_value=str(self.path) + "\n"), \
             patch.object(SNAPSHOTS.subprocess, "run"):
            with self.assertRaises(FileNotFoundError):
                SNAPSHOTS.reconcile(self.root, Path("producer"), refresh=True)
            self.assertTrue((self.directory / "forks.rs").exists())


if __name__ == "__main__":
    unittest.main()
