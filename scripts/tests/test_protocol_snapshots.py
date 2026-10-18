"""Protocol source refresh refuses producer drift and preserves authored files."""

import importlib.util
import contextlib
import io
import os
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

    def test_declared_tree_check_refuses_changed_missing_and_obsolete_mirrors(self):
        generated = self.root / "actual-generator-output"
        (generated / self.path).parent.mkdir(parents=True)
        (generated / self.path).write_bytes(self.contents)
        stamp = self.root / "check.stamp"
        arguments = ["--generated-root", str(generated), "--source-root", str(self.root),
                     "--check", "--stamp", str(stamp)]
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(SNAPSHOTS.main(arguments), 1)
            self.assertFalse(stamp.exists())
            (self.directory / "forks.rs").unlink()
            (self.root / self.path).write_bytes(self.contents)
            self.assertEqual(SNAPSHOTS.main(arguments), 0)
            self.assertTrue(stamp.is_file())
            stamp.unlink()
            (self.root / self.path).write_text("stale mirror\n")
            self.assertEqual(SNAPSHOTS.main(arguments), 1)
            self.assertFalse(stamp.exists())
            self.assertEqual((generated / self.path).read_bytes(), self.contents)
            self.assertEqual((self.root / self.path).read_text(), "stale mirror\n")

    def test_declared_tree_roster_refuses_missing_extra_and_refresh(self):
        generated = self.root / "actual-generator-output"
        generated.mkdir()
        arguments = ["--generated-root", str(generated), "--source-root", str(self.root)]
        with self.assertRaisesRegex(ValueError, "generated tree differs"):
            SNAPSHOTS.main(arguments + ["--check"])
        (generated / self.path).parent.mkdir(parents=True)
        (generated / self.path).write_bytes(self.contents)
        (generated / "unexpected.rs").write_text("not declared\n")
        with self.assertRaisesRegex(ValueError, "generated tree differs"):
            SNAPSHOTS.main(arguments + ["--check"])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            SNAPSHOTS.main(arguments + ["--refresh"])
        self.assertFalse((self.root / self.path).exists())


class DeclaredProtocolSnapshots(unittest.TestCase):
    def test_current_generator_output_and_isolated_stale_mirror(self):
        generated = Path(os.environ["TIDEPOOL_PROTOCOL_GENERATED_ROOT"])
        sources = Path(os.environ["TIDEPOOL_PROTOCOL_SNAPSHOT_ROOT"])
        paths = SNAPSHOTS.declared_paths(sources)
        self.assertGreater(len(paths), 0)
        self.assertEqual(SNAPSHOTS.compare(sources, generated, paths), ([], []))
        with tempfile.TemporaryDirectory() as temporary:
            isolated = Path(temporary)
            for path in paths:
                destination = isolated / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes((sources / path).read_bytes())
            marker = paths[0]
            original = (generated / marker).read_bytes()
            (isolated / marker).write_bytes(original + b"\nstale mirror only\n")
            self.assertEqual(SNAPSHOTS.compare(isolated, generated, paths), ([marker], []))
            self.assertEqual((generated / marker).read_bytes(), original)
            (isolated / marker).write_bytes(original)
            self.assertEqual(SNAPSHOTS.compare(isolated, generated, paths), ([], []))


if __name__ == "__main__":
    unittest.main()
