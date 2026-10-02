"""Retain and audit tracked and untracked Git source bytes for an acceptance run."""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tarfile


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args])


def excluded(path, prefixes):
    return any(path == prefix or path.startswith(prefix + '/') for prefix in prefixes)


def inventory(root, prefixes, base=''):
    paths = sorted(set(git(root, 'ls-files', '-c', '-o', '--exclude-standard', '-z').split(b'\0')) - {b''})
    links = {}
    for record in git(root, 'ls-files', '--stage', '-z').split(b'\0'):
        if not record:
            continue
        metadata, path = record.split(b'\t', 1)
        mode, oid, stage = metadata.split()
        if stage != b'0':
            raise RuntimeError('source has an unresolved index conflict: ' + os.fsdecode(path))
        if mode == b'160000':
            links[path] = oid.decode()
    for raw in paths:
        local = os.fsdecode(raw)
        path = base + local
        if excluded(path, prefixes):
            continue
        source = root / local
        if raw in links:
            if not (source / '.git').exists():
                raise RuntimeError('source submodule is not initialized: ' + path)
            yield {'path': path, 'kind': 'gitlink', 'index_oid': links[raw],
                   'head_oid': git(source, 'rev-parse', 'HEAD').decode().strip()}, None
            yield from inventory(source, prefixes, path + '/')
            continue
        try:
            info = source.lstat()
        except FileNotFoundError:
            yield {'path': path, 'kind': 'missing'}, None
            continue
        if stat.S_ISLNK(info.st_mode):
            content = os.fsencode(os.readlink(source))
            mode, kind = 0o120000, 'symlink'
        elif stat.S_ISREG(info.st_mode):
            content = source.read_bytes()
            mode, kind = (0o100755 if info.st_mode & 0o111 else 0o100644), 'file'
        else:
            raise RuntimeError('unsupported source file type: ' + path)
        yield {'path': path, 'kind': kind, 'mode': mode, 'bytes': len(content),
               'sha256': hashlib.sha256(content).hexdigest()}, content


def state(root, prefixes):
    return {'head_oid': git(root, 'rev-parse', 'HEAD').decode().strip(),
            'entries': [entry for entry, _ in inventory(root, prefixes)]}


def differences(before, after):
    changes = []
    if before['head_oid'] != after['head_oid']:
        changes.append({'path': '(HEAD)', 'before': before['head_oid'], 'after': after['head_oid']})
    left = {entry['path']: entry for entry in before['entries']}
    right = {entry['path']: entry for entry in after['entries']}
    for path in sorted(left.keys() | right.keys()):
        if left.get(path) != right.get(path):
            changes.append({'path': path, 'before': left.get(path), 'after': right.get(path)})
    return changes


def file_digest(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def repository_root(root):
    root = root.resolve()
    actual = Path(os.fsdecode(git(root, 'rev-parse', '--show-toplevel')).strip()).resolve()
    if root != actual:
        raise RuntimeError('source capture requires the complete repository root')
    return root


def capture(root, output, prefixes):
    root, output = repository_root(root), output.resolve()
    if output.is_relative_to(root):
        relative = str(output.relative_to(root))
        result = subprocess.run(['git', '-C', str(root), 'check-ignore', '--quiet', relative])
        if result.returncode != 0:
            raise RuntimeError('source snapshot output inside the repository must be Git-ignored')
    output.mkdir(parents=True, exist_ok=False)
    before = {'head_oid': git(root, 'rev-parse', 'HEAD').decode().strip(), 'entries': []}
    with tarfile.open(output / 'sources.tar', 'w') as archive:
        for entry, content in inventory(root, prefixes):
            before['entries'].append(entry)
            if content is None:
                continue
            member = tarfile.TarInfo(entry['path'])
            member.mode = entry['mode'] & 0o777
            if entry['kind'] == 'symlink':
                member.type = tarfile.SYMTYPE
                member.linkname = os.fsdecode(content)
                archive.addfile(member)
            else:
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))
    changes = differences(before, state(root, prefixes))
    manifest = {'version': 1, 'inventory_command': 'git ls-files -c -o --exclude-standard -z',
                'submodules': 'initialized submodule source inventories captured recursively',
                'exclude_prefixes': prefixes, 'source': before,
                'capture_changes': changes}
    manifest['archive_sha256'] = file_digest(output / 'sources.tar')
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    if changes:
        raise RuntimeError('source changed during capture; see manifest capture_changes')
    return manifest


def audit(root, snapshot, output):
    manifest = json.loads((snapshot / 'manifest.json').read_text())
    if manifest['version'] != 1 or manifest['capture_changes']:
        raise RuntimeError('snapshot is incomplete or changed during capture')
    if file_digest(snapshot / 'sources.tar') != manifest['archive_sha256']:
        raise RuntimeError('retained source archive digest changed')
    current = state(repository_root(root), manifest['exclude_prefixes'])
    report = {'source': current, 'changes': differences(manifest['source'], current)}
    output.write_text(json.dumps(report, indent=2) + '\n')
    return not report['changes']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    take = commands.add_parser('capture')
    take.add_argument('root', type=Path)
    take.add_argument('output', type=Path)
    take.add_argument('--exclude', action='append', default=[])
    check = commands.add_parser('audit')
    check.add_argument('root', type=Path)
    check.add_argument('snapshot', type=Path)
    check.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == 'capture':
            if any(not prefix or prefix.startswith('/') or '..' in Path(prefix).parts for prefix in args.exclude):
                raise RuntimeError('exclusions must be nonempty repository-relative path prefixes')
            manifest = capture(args.root, args.output, [prefix.rstrip('/') for prefix in args.exclude])
            print(f"Captured {len(manifest['source']['entries'])} source entries")
            return 0
        return 0 if audit(args.root, args.snapshot, args.output) else 1
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        parser.exit(2, f'source snapshot failed: {error}\n')


if __name__ == '__main__':
    raise SystemExit(main())
