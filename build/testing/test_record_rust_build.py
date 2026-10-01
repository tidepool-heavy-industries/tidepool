"""Optimized build selection and effective compiler environment boundaries."""

import importlib.util
import json
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location('record_rust_build', ROOT / 'record-rust-build.py')
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)


def artifact(name='tidepool', optimized='3', test=True, package='tidepool'):
    return {'reason': 'compiler-artifact', 'package_id': f'path+file:///src/facade#{package}@0.1.0',
            'target': {'name': name, 'kind': ['lib' if test else 'bin']},
            'profile': {'opt_level': optimized, 'test': test}, 'executable': '/build/output'}


class BuildEvidence(unittest.TestCase):
    def test_selects_actual_optimized_package_artifact(self):
        rows = [artifact(package='other'), artifact(name='other'), artifact()]
        self.assertEqual(BUILD.select_artifact('\n'.join(map(json.dumps, rows)),
                                              'tidepool', 'lib-test', 'tidepool'), rows[-1])

    def test_refuses_debug_duplicate_and_wrong_kind(self):
        for rows in ([artifact(optimized='0')], [artifact(), artifact()], [artifact(test=False)]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                BUILD.select_artifact('\n'.join(map(json.dumps, rows)), 'tidepool', 'lib-test', 'tidepool')

    def test_preserves_gc_flags_and_admits_output_directory(self):
        source = {'PATH': '/tools', 'CARGO_TARGET_DIR': '/build'}
        result = BUILD.build_environment(source, Path('/nix/store/tool/bin/rustc'),
                                         {'build': {'rustflags': ['-C', 'force-frame-pointers=yes']}})
        self.assertEqual(result['CARGO_ENCODED_RUSTFLAGS'], '-C\x1fforce-frame-pointers=yes')
        self.assertEqual(result['CARGO_TARGET_DIR'], '/build')
        self.assertEqual(result['RUSTC_WRAPPER'], '')
        self.assertNotIn('RUSTC', source)

    def test_refuses_compiler_and_profile_overrides(self):
        for key in ('RUSTC', 'RUSTFLAGS', 'RUSTC_WRAPPER', 'CARGO_PROFILE_RELEASE_OPT_LEVEL',
                    'CARGO_ENCODED_RUSTFLAGS', 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS'):
            with self.subTest(key=key), self.assertRaises(ValueError):
                BUILD.build_environment({key: 'override'}, Path('/nix/store/tool/bin/rustc'), {})

    def test_refuses_optimization_in_owning_flags(self):
        with self.assertRaises(ValueError):
            BUILD.build_environment({}, Path('/nix/store/tool/bin/rustc'),
                                    {'build': {'rustflags': ['-C', 'opt-level=0']}})


if __name__ == '__main__':
    unittest.main()
