#!/usr/bin/env python3
"""Build and freeze one optimized Cargo output with its actual build evidence."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tomllib

from source_snapshot import audit, capture, repository_root


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def reference(path):
    return {'path': str(Path(path).resolve()), 'sha256': digest(path)}


def select_artifact(messages, package, kind, name):
    candidates = []
    for line in messages.splitlines():
        row = json.loads(line)
        if row.get('reason') != 'compiler-artifact' or not row.get('executable'):
            continue
        target, profile = row['target'], row['profile']
        package_id = row['package_id']
        # Cargo's modern package ID ends in #name@version; path packages may
        # omit the name when the directory and package names coincide.
        owner = package_id.rsplit('#', 1)[-1].split('@', 1)[0]
        if owner != package:
            continue
        if target['name'] != name or profile['test'] != (kind == 'lib-test'):
            continue
        if ('lib' if kind == 'lib-test' else 'bin') not in target['kind']:
            continue
        if profile['opt_level'] not in ('2', '3', 's', 'z'):
            raise ValueError('selected Cargo artifact is not optimized')
        candidates.append(row)
    if len(candidates) != 1:
        raise ValueError(f'expected one selected executable, found {len(candidates)}')
    return candidates[0]


def build_environment(environment, rustc, config):
    forbidden = {'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS'}
    overrides = [key for key, value in environment.items() if value and (
        key in forbidden or key.startswith(('CARGO_PROFILE_', 'CARGO_BUILD_RUST'))
        or (key.startswith('CARGO_TARGET_') and key.endswith(('_RUSTFLAGS', '_LINKER', '_RUNNER'))))]
    if overrides:
        raise ValueError('inherited compiler/profile overrides: ' + ', '.join(sorted(overrides)))
    flags = config.get('build', {}).get('rustflags', [])
    if not isinstance(flags, list) or any(not isinstance(flag, str) for flag in flags):
        raise ValueError('owning build.rustflags must be an argument array')
    if any('opt-level' in flag or 'codegen-units' in flag for flag in flags):
        raise ValueError('owning Rust flags override the selected optimization profile')
    result = environment.copy()
    result.update(RUSTC=str(rustc), RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='',
                  CARGO_ENCODED_RUSTFLAGS='\x1f'.join(flags))
    result.pop('RUSTFLAGS', None)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', required=True)
    parser.add_argument('--kind', choices=('lib-test', 'bin'), required=True)
    parser.add_argument('--name', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    root = repository_root(Path.cwd())
    output = args.output.resolve()
    if not output.is_relative_to(root) or subprocess.run(
            ['git', 'check-ignore', '--quiet', str(output.relative_to(root))]).returncode:
        parser.error('output must be a fresh Git-ignored repository directory')
    if output.exists():
        parser.error('output already exists; retained build evidence cannot be overwritten')
    tools = {name: Path(shutil.which(name) or '').resolve() for name in ('cargo', 'rustc')}
    if not os.environ.get('IN_NIX_SHELL') or any(
            not str(path).startswith('/nix/store/') or not path.is_file() for path in tools.values()):
        parser.error('run through the declared Nix shell with its materialized Cargo and Rustc')
    config_path = root / '.cargo/config.toml'
    environment = build_environment(os.environ, tools['rustc'], tomllib.loads(config_path.read_text()))
    output.mkdir()
    packet = {'schema': 'cargo-build-v1', 'compiler': reference(tools['rustc']),
              'cargo': reference(tools['cargo']), 'config': reference(config_path)}
    (output / 'build-flags.json').write_text(json.dumps({key: environment[key] for key in (
        'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_ENCODED_RUSTFLAGS')}, indent=2) + '\n')
    packet['build_flags'] = reference(output / 'build-flags.json')
    try:
        manifest = capture(root, output / 'source-before', ['test-source-boot'])
        packet['source_oid'] = manifest['source']['head_oid']
        command = [str(tools['cargo']), 'test' if args.kind == 'lib-test' else 'build',
                   '--release', '--locked', '--offline', '--no-default-features', '-p', args.package,
                   '--message-format=json-render-diagnostics', '-j', '2']
        command += ['--lib', '--no-run'] if args.kind == 'lib-test' else ['--bin', args.name]
        packet['command'] = command
        with (output / 'cargo-messages.jsonl').open('wb') as messages, (output / 'build.log').open('wb') as log:
            status = subprocess.run(command, env=environment, stdout=messages, stderr=log).returncode
        packet['exit_code'] = status
        packet['cargo_messages'] = reference(output / 'cargo-messages.jsonl')
        packet['build_log'] = reference(output / 'build.log')
        if status:
            raise RuntimeError(f'Cargo exited {status}; retained build.log contains diagnostics')
        artifact = select_artifact((output / 'cargo-messages.jsonl').read_text(), args.package, args.kind, args.name)
        original = Path(artifact['executable'])
        destination = output / args.name
        original_digest = digest(original)
        shutil.copy2(original, destination)
        destination.chmod(0o555)
        if digest(destination) != original_digest:
            raise RuntimeError('compiler output changed while being frozen')
        packet['output'] = dict(reference(destination), cargo_path=str(original))
        if not audit(root, output / 'source-before', output / 'source-after.json'):
            raise RuntimeError('source changed during build; see retained source-after.json')
        packet['source_before'] = reference(output / 'source-before/manifest.json')
        packet['source_after'] = reference(output / 'source-after.json')
        packet['source_archive'] = reference(output / 'source-before/sources.tar')
        packet['status'] = 'built'
    except Exception as error:
        packet['status'] = 'refused'
        packet['error'] = str(error)
        raise
    finally:
        (output / 'packet.json').write_text(json.dumps(packet, indent=2) + '\n')


if __name__ == '__main__':
    main()
