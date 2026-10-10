"""Process regressions for thin native operator command orchestration."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]


class NativeOperatorFrontends(unittest.TestCase):
    def test_m1_acceptance_selects_descriptor_owner_and_rejects_legacy_pair(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shared = root / 'bundle/share/exomonad'
            shared.mkdir(parents=True)
            owner = shared / 'qualification.py'
            owner.write_text("import json, pathlib, sys\n"
                             "args = sys.argv[1:]\n"
                             "assert args[0] == 'run'\n"
                             "pathlib.Path(args[args.index('--output') + 1]).write_text(json.dumps(args))\n")
            descriptor = shared / 'qualification.json'
            descriptor.write_text('{}\n')
            output = root / 'receipt'
            script = REPO / 'build/testing/run-m1-acceptance.sh'
            result = subprocess.run(['bash', str(script), str(descriptor), str(output)],
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(output.read_text()), ['run', str(descriptor),
                                                             '--cohort', 'm1', '--output', str(output)])
            legacy = subprocess.run(['bash', str(script), str(root / 'bundle'),
                                     str(descriptor), str(root / 'legacy')],
                                    text=True, capture_output=True, timeout=10)
            self.assertNotEqual(legacy.returncode, 0)
            self.assertFalse((root / 'legacy').exists())

    def test_main_acceptance_preserves_failure_and_attempts_all_native_owners(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            frontend = root / 'bridge/haskell/test-check/run-main-acceptance.sh'
            frontend.parent.mkdir(parents=True)
            shutil.copyfile(REPO / frontend.relative_to(root), frontend)
            (root / 'scripts').mkdir()
            buck = root / 'scripts/buck2-run.sh'
            buck.write_text("""#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$RECORD"
[[ "$*" != *facade_prepared_recipe_contract_test* ]] || exit 7
""")
            output = root / 'evidence'
            record = root / 'commands'
            env = os.environ | {'RECORD': str(record)}
            result = subprocess.run(['bash', str(frontend), str(output)],
                                    env=env, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            commands = record.read_text().splitlines()
            self.assertEqual(len(commands), 6)
            self.assertTrue(all(command.startswith('run --local-only -c remote.enabled=false //')
                                for command in commands))
            self.assertEqual((output / 'facade_prepared_recipe_contract_test.log.status').read_text(), '7\n')
            self.assertEqual((output / 'browser_scenario_contract.log.status').read_text(), '0\n')
            repeated = subprocess.run(['bash', str(frontend), str(output)],
                                      env=env, text=True, capture_output=True, timeout=10)
            self.assertEqual(repeated.returncode, 2)
            self.assertEqual(len(record.read_text().splitlines()), 6)

    def test_delegated_resources_keeps_caller_evidence_path_when_buck_changes_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            frontend = root / 'repo/exomonad/scripts/test-command-resources-delegated.sh'
            frontend.parent.mkdir(parents=True)
            shutil.copyfile(REPO / 'exomonad/scripts/test-command-resources-delegated.sh', frontend)
            (root / 'repo/scripts').mkdir()
            buck = root / 'repo/scripts/buck2-run.sh'
            buck.write_text("""#!/usr/bin/env bash
cd "$(dirname "$0")/.."
while [[ $# -gt 0 ]]; do
  if [[ "$1" == --output-dir ]]; then
    mkdir "$2"
    pwd > "$2/launch-directory"
    exit 0
  fi
  shift
done
exit 8
""")
            result = subprocess.run(['bash', str(frontend), 'evidence'], cwd=root,
                                    text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertTrue((root / 'evidence/launch-directory').is_file())
            self.assertFalse((root / 'repo/evidence').exists())

    def test_recipe_checks_keep_frozen_owner_receipts_and_fail_on_actual_child_status(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            workspace = root / 'workspace'
            (workspace / '.exomonad').mkdir(parents=True)
            config = workspace / '.exomonad/config.toml'
            config.write_text('[haskell]\nchecks = ["Project.pass", "Project.fail"]\n')
            bundle = root / 'bundle'
            shared = bundle / 'share/exomonad'
            shared.mkdir(parents=True)
            owner = shared / 'qualification.py'
            owner.write_text("""import json, pathlib, sys
assert sys.argv[1:3] == ['exec', '--report']
pathlib.Path(sys.argv[3]).write_text(json.dumps(sys.argv[4:]))
raise SystemExit(9 if sys.argv[-1] == 'Project.fail' else 0)
""")
            descriptor = shared / 'qualification.json'
            descriptor.write_text('{}\n')
            reports = root / 'evidence'
            command = ['bash', str(REPO / 'exomonad/scripts/exomonad-check-recipes.sh'),
                       str(descriptor), str(reports), str(workspace), '2']
            result = subprocess.run(command, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 9, result.stdout + result.stderr)
            receipts = [json.loads(path.read_text()) for path in reports.glob('*.process.json')]
            self.assertEqual(len(receipts), 2)
            self.assertTrue(all(receipt[:3] == [str(descriptor), '--', 'check'] for receipt in receipts))
            self.assertEqual({receipt[-1] for receipt in receipts}, {'Project.pass', 'Project.fail'})
            legacy_reports = root / 'legacy-evidence'
            legacy = subprocess.run(['bash', str(REPO / 'exomonad/scripts/exomonad-check-recipes.sh'),
                                     str(bundle), str(descriptor), str(legacy_reports), str(workspace), '2'],
                                    text=True, capture_output=True, timeout=10)
            self.assertNotEqual(legacy.returncode, 0)
            self.assertEqual(list(legacy_reports.glob('*.process.json')), [])
            config.write_text('[haskell]\nchecks = ["Project.pass", "Project.pass"]\n')
            rejected = root / 'rejected'
            command[2] = str(rejected)
            result = subprocess.run(command, text=True, capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(list(rejected.glob('*.process.json')), [])


if __name__ == '__main__':
    unittest.main()
