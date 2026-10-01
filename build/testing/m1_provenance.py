#!/usr/bin/env python3
"""Bounded admission of trusted producer evidence and M1 browser inputs.

The caller admits the producer packet by SHA256. This checks consistency of
that retained build record; it does not independently reproduce the binary.
Frontend baseline authority is its separately admitted digest and the owning
frontend's deployment manifest, not a claim about current Rust source.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys

MAX_JSON = 16 * 1024 * 1024
MAX_FILES = 32768
MAX_BYTES = 1024 * 1024 * 1024
WORKER_CONFIG = ('bridge/haskell/cabal.project', 'bridge/haskell/cabal.project.freeze')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    path = Path(path)
    require(path.is_file() and path.stat().st_size <= MAX_BYTES, f'invalid or oversized file: {path}')
    with path.open('rb') as stream:
        result = hashlib.file_digest(stream, 'sha256').hexdigest()
    return result


def checked_digest(value):
    require(isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value), 'invalid SHA256')
    return value


def read_json(path):
    require(Path(path).stat().st_size <= MAX_JSON, f'oversized JSON: {path}')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, f'duplicate JSON key: {key}')
            result[key] = value
        return result
    return json.loads(Path(path).read_text(), object_pairs_hook=unique)


def relative(value):
    require(isinstance(value, str) and value and '\\' not in value and not any(ord(c) < 32 for c in value), 'invalid relative evidence path')
    path = PurePosixPath(value)
    require(not path.is_absolute() and all(p not in ('..', '.') for p in path.parts), 'unsafe evidence path')
    return path


def referenced(base, value):
    path = base / relative(value)
    require(path.resolve().is_relative_to(base.resolve()), 'evidence reference escapes packet directory')
    return path


def worker_source_owner():
    # Campaign controls retain the existing source enumerator beside this helper.
    frozen = Path(__file__).resolve().with_name('lib-extract.sh')
    return frozen if frozen.is_file() else Path(__file__).resolve().parents[2] / 'scripts/lib-extract.sh'


def worker_roots(root):
    result = subprocess.run(['bash', '-c', 'source "$1"; tidepool_extract_worker_sources',
                             'worker-source-owner', str(worker_source_owner())],
                            cwd=root, check=True, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=30)
    require(len(result.stdout) <= 65536, 'worker source enumerator exceeds bound')
    roots = []
    for value in result.stdout.splitlines():
        path = Path(value)
        require(path.is_absolute() and path.is_relative_to(root), 'worker source enumerator escaped root')
        roots.append(str(relative(path.relative_to(root).as_posix())))
    require(roots and len(roots) <= 128 and len(roots) == len(set(roots)), 'invalid worker source enumeration')
    return roots + list(WORKER_CONFIG)


def worker_inputs(root, roots):
    paths = set()
    for key in roots:
        base = root / key
        require(base.exists() and not base.is_symlink(), f'missing or symlink worker input: {key}')
        if base.is_file():
            paths.add(key)
            continue
        require(base.is_dir(), 'nonregular worker input')
        for parent, dirs, files in os.walk(base, followlinks=False):
            require(not any((Path(parent) / name).is_symlink() for name in dirs), 'symlink worker source directory')
            for name in files:
                path = Path(parent) / name
                require(not path.is_symlink(), 'symlink worker source file')
                paths.add(path.relative_to(root).as_posix())
                require(len(paths) <= MAX_FILES, 'worker input count exceeds bound')
    return paths


def validate_sources(root, packet_dir, packet, retain):
    manifest_path = referenced(packet_dir, packet['all_haskell_source_hashes'])
    require(digest(manifest_path) == checked_digest(packet['source_manifest_sha256']), 'producer source manifest digest mismatch')
    manifest = read_json(manifest_path)
    require(manifest['head'] == packet['git_oid'], 'producer source revision mismatch')
    rows = manifest['files']
    require(isinstance(rows, list) and len(rows) <= MAX_FILES, 'producer source manifest count exceeds bound')
    by_path = {}
    for row in rows:
        key = str(relative(row['path']))
        require(key not in by_path, 'duplicate producer source path')
        checked_digest(row['sha256'])
        require(type(row['bytes']) is int and 0 <= row['bytes'] <= MAX_BYTES, 'invalid source size')
        by_path[key] = row
    roots = worker_roots(root)
    relevant = {key for key in by_path if any(key == owner or key.startswith(owner + '/') for owner in roots)}
    require(relevant == worker_inputs(root, roots), 'producer worker input inventory differs from current source')
    archive = referenced(packet_dir, packet['all_haskell_source_bytes'])
    total = 0
    for key in sorted(relevant):
        row = by_path[key]
        original = referenced(archive, key)
        current = root / key
        total += row['bytes']
        require(total <= MAX_BYTES, 'worker source byte count exceeds bound')
        require(original.stat().st_size == row['bytes'] and digest(original) == row['sha256'], f'retained worker source mismatch: {key}')
        require(current.stat().st_size == row['bytes'] and digest(current) == row['sha256'], f'current worker source mismatch: {key}')
        target = retain / 'worker-source' / key
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(original, target)
        require(digest(target) == row['sha256'], f'worker source changed during retention: {key}')
    shutil.copyfile(manifest_path, retain / 'source-manifest.json')
    require(digest(retain / 'source-manifest.json') == packet['source_manifest_sha256'], 'producer source manifest changed during retention')
    return {'files': len(relevant), 'bytes': total, 'manifest_sha256': digest(manifest_path),
            'roots': roots, 'entries': [{'path': key, 'kind': 'file', 'bytes': by_path[key]['bytes'],
                                       'sha256': by_path[key]['sha256']} for key in sorted(relevant)]}


def command(args):
    result = subprocess.run(args, check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
    require(len(result.stdout) <= 65536 and len(result.stderr) <= 65536, 'tool version output exceeds bound')
    return result.stdout.strip()


def immutable(path):
    resolved = Path(path).resolve(strict=True)
    require(resolved.is_relative_to('/nix/store') and len(resolved.parts) >= 4, f'input is not in immutable Nix store: {path}')
    return str(resolved)


def active_tools(env):
    for key in ('RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC',
                'CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'):
        require(not env.get(key), f'inherited compiler override is not admitted: {key}')
    flake = env['TIDEPOOL_DEV_FLAKE']
    require(re.fullmatch(r'git\+[^\s]+\?[^\s]*rev=[0-9a-f]{40}(?:&[^\s]*)?|/nix/store/[^\s]+', flake), 'development flake is not immutable')
    require(env['TIDEPOOL_DEV_SHELL'] == flake + '#default', 'active shell differs from pinned default shell')
    tools = {}
    for name, flag in (('ghc', '--version'), ('rustc', '--version'), ('cabal', '--version')):
        selected = shutil.which(name)
        require(selected is not None, f'missing pinned tool: {name}')
        if name in ('ghc', 'rustc'):
            require(Path(selected).resolve() == Path(env['TIDEPOOL_DEV_' + name.upper()]).resolve(), f'active {name} differs from dev-shell selection')
        path = immutable(selected)
        tools[name] = {'path': path, 'sha256': digest(path), 'version': command([path, flag])}
    return tools


def inventory(path):
    """Inventory logical served names, symlink identities and dereferenced bytes."""
    requested = Path(path).absolute()
    root = requested.resolve(strict=True)
    require(root.is_dir(), 'inventory root is not a directory')
    entries = []
    total = 0
    def visit(actual, logical, ancestors):
        nonlocal total
        require(len(entries) < MAX_FILES, 'inventory entry count exceeds bound')
        resolved = actual.resolve(strict=True)
        row = {'path': logical, 'resolved': str(resolved)}
        if actual.is_symlink():
            row['symlink'] = os.readlink(actual)
        if resolved.is_dir():
            require(resolved not in ancestors, 'cyclic directory symlink')
            row['kind'] = 'directory'
            entries.append(row)
            children = []
            for child in resolved.iterdir():
                require(len(entries) + len(children) < MAX_FILES, 'inventory entry count exceeds bound')
                children.append(child)
            for child in sorted(children):
                visit(child, logical + '/' + child.name if logical else child.name, ancestors | {resolved})
        else:
            require(resolved.is_file(), 'nonregular served input')
            size = resolved.stat().st_size
            total += size
            require(total <= MAX_BYTES, 'inventory byte count exceeds bound')
            row.update(kind='file', bytes=size, sha256=digest(resolved))
            entries.append(row)
    visit(requested, '', set())
    require(any(row['kind'] == 'file' for row in entries), 'empty input inventory')
    return {'root': str(requested), 'resolved_root': str(root), 'entries': entries, 'bytes': total}


def identity(value):
    require(isinstance(value, list) and len(value) == 32 and all(type(v) is int and 0 <= v <= 255 for v in value), 'invalid deployment identity')
    return bytes(value).hex()


def validate_packet(packet_path, expected_packet_sha, frontend, worker, frontend_sha, worker_sha):
    require(digest(packet_path) == checked_digest(expected_packet_sha), 'producer packet is not the admitted packet')
    packet = read_json(packet_path)
    for name, path, expected in (('frontend', frontend, frontend_sha), ('worker', worker, worker_sha)):
        require(digest(path) == checked_digest(expected) == packet[name + '_sha256'], f'{name} digest disagrees with producer record')
    require(type(packet['build']['exit_status']) is int and packet['build']['exit_status'] == 0, 'producer build did not succeed')
    return packet


def admit(root, output, env):
    packet_path = Path(env['M1_PRODUCER_EVIDENCE']).resolve(strict=True)
    frontend, worker = output / 'frozen-bin/tidepool-extract', output / 'frozen-bin/tidepool-extract-bin'
    require(packet_path.stat().st_size <= MAX_JSON, 'producer packet exceeds JSON bound')
    frozen_packet = output / 'producer-provenance.json'
    shutil.copyfile(packet_path, frozen_packet)
    packet = validate_packet(frozen_packet, env['M1_PRODUCER_EVIDENCE_SHA256'], frontend, worker,
                             env['M1_FRONTEND_SHA256'], env['M1_WORKER_SHA256'])
    tools = active_tools(env)
    build = packet['build']
    require(build['flake'] == env['TIDEPOOL_DEV_FLAKE'], 'producer build used a different pinned flake')
    require(build['tools'] == tools, 'producer tool audit differs from active pinned tools')
    require(build['tool_record_origin'] in ('build_invocation', 'post_build_same_pinned_shell_audit'), 'unknown producer tool audit origin')
    retain = output / 'producer-record'
    retain.mkdir()
    sources = validate_sources(root, packet_path.parent, packet, retain)
    log = referenced(packet_path.parent, build['log'])
    require(digest(log) == checked_digest(build['log_sha256']), 'producer build log digest mismatch')
    shutil.copyfile(log, retain / 'build.log')
    require(digest(retain / 'build.log') == build['log_sha256'], 'producer build log changed during retention')
    shutil.copyfile(referenced(packet_path.parent, packet['manifest']), retain / 'recorded-deployment.json')
    original = read_json(retain / 'recorded-deployment.json')
    ghc_libdir = command([tools['ghc']['path'], '--print-libdir'])
    require(original['schema'] == 1 and original['ghc_libdir'] == ghc_libdir, 'producer deployment GHC libdir mismatch')
    require(identity(original['producer_identity']) == packet['producer_identity'] and identity(original['consumed_worker_identity']) == packet['consumed_worker_identity'], 'producer deployment identity mismatch')
    deployment = output / 'compiler-deployment.json'
    clean = dict(env, TIDEPOOL_EXTRACT_WORKER=str(worker), TIDEPOOL_GHC_LIBDIR=ghc_libdir)
    clean.pop('TIDEPOOL_ALLOW_STALE_EXTRACT', None)
    subprocess.run([str(frontend), '--compiler-deployment-manifest', str(deployment)], env=clean, check=True, timeout=30)
    current = read_json(deployment)
    require(current['schema'] == 1 and current['producer_identity'] == original['producer_identity'] and current['consumed_worker_identity'] == original['consumed_worker_identity'] and current['ghc_libdir'] == ghc_libdir, 'frozen binaries do not reproduce recorded deployment authority')
    require(Path(current['frontend_path']).resolve() == frontend.resolve() and Path(current['worker_path']).resolve() == worker.resolve(), 'deployment selected unfrozen binary')
    node = immutable(env['TIDEPOOL_BROWSER_NODE'])
    browser_root = immutable(env['PLAYWRIGHT_BROWSERS_PATH'])
    require(Path(browser_root).is_dir(), 'browser closure is not a directory')
    dependencies = inventory(env['M1_BROWSER_NODE_MODULES'])
    package = Path(env['M1_BROWSER_NODE_MODULES']) / 'playwright-core/package.json'
    require(package.is_file(), 'playwright-core package is missing')
    browser = command([node, '-e', 'const p=require(process.argv[1]); console.log(p.chromium.executablePath())', str(package.parent)])
    browser_path = immutable(browser)
    require(Path(browser).absolute().is_relative_to(Path(env['PLAYWRIGHT_BROWSERS_PATH']).absolute()), 'Playwright browser is outside admitted browser closure')
    lock = Path(env['M1_BROWSER_PACKAGE_LOCK']).resolve(strict=True)
    locked = read_json(lock)
    require(locked['packages']['node_modules/playwright-core']['version'] == read_json(package)['version'], 'playwright-core differs from admitted package lock')
    shutil.copyfile(lock, output / 'browser-package-lock.json')
    report = {'schema': 1, 'authority': 'caller-admitted trusted build record; not independent binary reproduction',
              'producer_packet_sha256': env['M1_PRODUCER_EVIDENCE_SHA256'], 'worker_sources': sources,
              'flake': env['TIDEPOOL_DEV_FLAKE'], 'shell': env['TIDEPOOL_DEV_SHELL'], 'tools': tools,
              'worker_source_owner_sha256': digest(worker_source_owner()),
              'cargo_target_dir': env['CARGO_TARGET_DIR'], 'rustflags': env.get('RUSTFLAGS', ''),
              'cargo_encoded_rustflags': env.get('CARGO_ENCODED_RUSTFLAGS', ''),
              'compiler_route': {'RUSTC': tools['rustc']['path'], 'RUSTC_WRAPPER': '', 'RUSTC_WORKSPACE_WRAPPER': ''},
              'cargo_build_jobs': env.get('CARGO_BUILD_JOBS'), 'ghc_libdir': ghc_libdir,
              'assets': inventory(env['EXOMONAD_EMBEDDED_ASSET_ROOT']), 'dependencies': dependencies,
              'node': {'path': node, 'sha256': digest(node), 'version': command([node, '--version'])},
              'browser': {'selection': 'chromium.executablePath; headless implementation also bound by immutable browser closure', 'path': browser_path, 'sha256': digest(browser_path), 'version': command([browser_path, '--version']), 'closure': browser_root},
              'playwright_core': {'path': str(package.resolve()), 'sha256': digest(package), 'version': read_json(package)['version']},
              'package_lock': {'path': str(lock), 'sha256': digest(lock)}}
    (output / 'provenance-admission.json').write_text(json.dumps(report, indent=2, sort_keys=True) + '\n')
    # Bind every retained evidence file, not only the top-level packet.
    files = [output / 'producer-provenance.json', output / 'browser-package-lock.json', deployment, output / 'provenance-admission.json'] + sorted(p for p in retain.rglob('*') if p.is_file())
    (output / 'provenance-inputs.sha256').write_text(''.join(f'{digest(p)}  {p}\n' for p in files))



def bind_snapshot(root, output):
    """Bind the captured baseline to admission, without substituting live bytes."""
    report = read_json(output / 'provenance-admission.json')
    manifest_path = output / 'source-before/manifest.json'
    snapshot = read_json(manifest_path)
    require(snapshot['version'] == 1 and snapshot['capture_changes'] == [], 'source snapshot is incomplete')
    require(digest(worker_source_owner()) == report['worker_source_owner_sha256'], 'admitted worker source enumerator changed')
    roots = worker_roots(root)
    require(roots == report['worker_sources']['roots'], 'admitted worker source roots changed')
    entries = snapshot['source']['entries']
    require(isinstance(entries, list) and len(entries) <= MAX_FILES, 'source snapshot count exceeds bound')
    by_path = {}
    for row in entries:
        key = str(relative(row['path']))
        require(key not in by_path, 'duplicate source snapshot path')
        by_path[key] = row
    enumerator = by_path.get('scripts/lib-extract.sh', {})
    require(enumerator.get('kind') == 'file' and enumerator.get('sha256') == report['worker_source_owner_sha256'],
            'captured worker source enumerator differs from admission')
    captured = {key: row for key, row in by_path.items()
                if any(key == owner or key.startswith(owner + '/') for owner in roots)}
    admitted = {row['path']: row for row in report['worker_sources']['entries']}
    require(len(admitted) == report['worker_sources']['files'], 'admitted worker inventory count mismatch')
    require(captured.keys() == admitted.keys(), 'captured worker input inventory differs from admission')
    for key, expected in admitted.items():
        row = captured[key]
        require(all(row.get(field) == expected[field] for field in ('kind', 'bytes', 'sha256')),
                f'captured worker input differs from admission: {key}')
        retained = output / 'producer-record/worker-source' / relative(key)
        require(retained.is_file() and not retained.is_symlink() and retained.stat().st_size == expected['bytes']
                and digest(retained) == expected['sha256'], f'admitted retained worker input changed: {key}')
    binding = {'schema': 1, 'producer_packet_sha256': report['producer_packet_sha256'],
               'source_snapshot_manifest_sha256': digest(manifest_path),
               'worker_source_owner_sha256': report['worker_source_owner_sha256'],
               'worker_inputs': len(admitted)}
    destination = output / 'worker-snapshot-binding.json'
    destination.write_text(json.dumps(binding, indent=2, sort_keys=True) + '\n')
    with (output / 'provenance-inputs.sha256').open('a') as hashes:
        hashes.write(f'{digest(destination)}  {destination}\n{binding["source_snapshot_manifest_sha256"]}  {manifest_path}\n')
    return binding


def audit(output):
    report = read_json(output / 'provenance-admission.json')
    for key in ('assets', 'dependencies'):
        require(inventory(report[key]['root']) == report[key], f'{key} inventory changed')
    for key in ('node', 'browser', 'playwright_core', 'package_lock'):
        require(digest(report[key]['path']) == report[key]['sha256'], f'{key} identity changed')
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('admit', 'bind-snapshot', 'audit'))
    parser.add_argument('--root', type=Path, default=Path.cwd())
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.action == 'admit':
            admit(args.root.resolve(), args.output.resolve(), os.environ)
        elif args.action == 'bind-snapshot':
            bind_snapshot(args.root.resolve(), args.output.resolve())
        else:
            audit(args.output.resolve())
    except (ValueError, KeyError, TypeError, AttributeError, IndexError, OSError, subprocess.SubprocessError) as error:
        print(f'M1 provenance refused: {error}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
