#!/usr/bin/env python3
"""Build curated graph-data.json from GitHub PR data with synthetic phase groupings."""
import json
import re
import subprocess
import sys
from datetime import datetime

# === Fetch PR data ===
print("Fetching PRs...")
raw = subprocess.check_output([
    'gh', 'pr', 'list', '--state', 'all', '--limit', '200',
    '--json', 'number,title,body,state,headRefName,baseRefName,additions,deletions,mergedAt,closedAt,url'
], text=True)
all_prs = json.loads(raw)

# Only merged PRs with main.* branches
prs = [p for p in all_prs if p['state'] == 'MERGED' and p['headRefName'].startswith('main.')]
prs.sort(key=lambda p: p['mergedAt'] or '')

print(f"  {len(prs)} merged PRs")

# === Reparenting: which flat branches belong under which synthetic parent ===
reparent = {
    # Phase 1: core-repr
    'main.types-and-datacon':    'main.core-repr',
    'main.pretty':               'main.core-repr',
    'main.serial':               'main.core-repr',
    'main.frame-and-utils':      'main.core-repr',
    'main.haskell-harness-impl': 'main.core-repr',
    'main.haskell-harness':      'main.core-repr',
    # Phase 2: core-eval
    'main.eval-strict-case':     'main.core-eval',
    'main.eval-thunks-joins':    'main.core-eval',
    # Phase 2: core-heap
    'main.heap-arena':           'main.core-heap',
    'main.gc-trace':             'main.core-heap',
    'main.gc-compact':           'main.core-heap',
    # Phase 2: core-bridge
    'main.bridge-scaffold':      'main.core-bridge',
    'main.bridge-derive':        'main.core-bridge',
    'main.haskell-macro':        'main.core-bridge',
    # Phase 2: core-testing
    'main.testing-generators':   'main.core-testing',
    'main.testing-oracle':       'main.core-testing',
    'main.testing-benchmarks':   'main.core-testing',
    # codegen-primops belongs under codegen
    'main.codegen-primops':      'main.codegen',
    # Extras: tide
    'main.tide-haskell':         'main.tide',
    'main.tide-parser':          'main.tide',
    # Runtime
    'main.tidepool-runtime':     'main.tide',
    # PRs that actually targeted main but should target their dot-parent
    'main.codegen.scaffold':     'main.codegen',
    'main.core-optimize.case-reduce': 'main.core-optimize',
    'main.core-optimize.beta-reduce': 'main.core-optimize',
}

# === Build nodes from PRs ===
nodes = {}

def ensure_node(branch_id):
    if branch_id in nodes:
        return
    parts = branch_id.split('.')
    label = parts[-1] if len(parts) > 1 else branch_id
    parent = '.'.join(parts[:-1]) if len(parts) > 1 else None
    # Apply reparenting
    if branch_id in reparent:
        parent = reparent[branch_id]
    nodes[branch_id] = {
        'id': branch_id,
        'parent': parent,
        'depth': 0,  # computed later
        'label': label,
        'prs': [],
        'additions': 0,
        'deletions': 0,
        'status': 'merged' if branch_id != 'main' else 'root',
    }

# Root
ensure_node('main')

# Synthetic grouping nodes (these never had their own PRs)
synthetics = ['main.core-repr', 'main.core-eval', 'main.core-heap',
              'main.core-bridge', 'main.core-testing', 'main.tide']
for s in synthetics:
    ensure_node(s)

# Create nodes from PR head branches
for pr in prs:
    head = pr['headRefName']
    ensure_node(head)

    pr_data = {
        'number': pr['number'],
        'title': pr['title'],
        'body': pr['body'],
        'state': 'merged',
        'url': pr['url'],
        'additions': pr['additions'],
        'deletions': pr['deletions'],
        'mergedAt': pr['mergedAt'],
        'closedAt': pr['closedAt'],
    }
    nodes[head]['prs'].append(pr_data)
    nodes[head]['additions'] += pr['additions']
    nodes[head]['deletions'] += pr['deletions']

# === Give synthetic nodes a mergedAt from their latest child ===
for syn_id in synthetics:
    children = [n for n in nodes.values() if n['parent'] == syn_id]
    child_merges = []
    for c in children:
        for p in c['prs']:
            if p['mergedAt']:
                child_merges.append(p['mergedAt'])
    if child_merges:
        latest = max(child_merges)
        nodes[syn_id]['prs'] = [{
            'number': 0,
            'title': f'{nodes[syn_id]["label"]} (phase grouping)',
            'body': '',
            'state': 'merged',
            'url': '',
            'additions': 0,
            'deletions': 0,
            'mergedAt': latest,
            'closedAt': latest,
        }]

# === Fetch ALL first-parent commits on main and route to ideal parent ===
print("Fetching commits on main...")
git_log = subprocess.check_output(
    ['git', 'log', '--first-parent', '--format=%H %aI %s', 'main'],
    text=True, cwd='..'
)

# PR number → headRefName for routing merges to ideal parent
pr_branch = {}
for p in prs:
    pr_branch[p['number']] = p['headRefName']

known_pr_nums = set(pr_branch.keys())

# Find ideal parent for a branch (reparent map, then dot-parent)
def ideal_parent(branch):
    if branch in reparent:
        return reparent[branch]
    parts = branch.split('.')
    return '.'.join(parts[:-1]) if len(parts) > 1 else None

# Find the latest PR merge timestamp to exclude viz/post-project commits
latest_pr_merge = None
for p in prs:
    if p['mergedAt']:
        if latest_pr_merge is None or p['mergedAt'] > latest_pr_merge:
            latest_pr_merge = p['mergedAt']

# Route each commit to the right node's directCommits
parent_commits = {}  # node_id → [commit]
for line in git_log.strip().split('\n'):
    if not line.strip():
        continue
    parts = line.split(' ', 2)
    sha, timestamp, message = parts[0], parts[1], parts[2]

    # Skip commits after latest PR merge (viz commit etc.)
    if latest_pr_merge and timestamp > latest_pr_merge:
        continue

    # Extract PR number from squash merge "(#N)" or merge commit "Merge pull request #N"
    pr_num = None
    m = re.search(r'\(#(\d+)\)', message)
    if m and int(m.group(1)) in known_pr_nums:
        pr_num = int(m.group(1))
    if pr_num is None:
        m = re.search(r'Merge pull request #(\d+)', message)
        if m and int(m.group(1)) in known_pr_nums:
            pr_num = int(m.group(1))

    if pr_num is not None:
        # PR merge/squash → route to the ideal parent of the PR's branch
        branch = pr_branch[pr_num]
        target = ideal_parent(branch)
        kind = 'merge'
    else:
        # Direct TL work → stays on main
        target = 'main'
        kind = 'direct'

    # Only route to nodes that exist
    if target not in nodes:
        target = 'main'

    if target not in parent_commits:
        parent_commits[target] = []
    parent_commits[target].append({
        'sha': sha[:7],
        'message': message,
        'timestamp': timestamp,
        'kind': kind,
    })

# Attach sorted directCommits to each node
for node_id, commits in parent_commits.items():
    commits.sort(key=lambda c: c['timestamp'])
    nodes[node_id]['directCommits'] = commits

total_dc = sum(len(c) for c in parent_commits.values())
print(f"  {total_dc} commits routed to {len(parent_commits)} nodes:")
for nid in sorted(parent_commits.keys()):
    dc = parent_commits[nid]
    n_direct = sum(1 for c in dc if c['kind'] == 'direct')
    n_merge = sum(1 for c in dc if c['kind'] == 'merge')
    parts = []
    if n_direct: parts.append(f"{n_direct} direct")
    if n_merge: parts.append(f"{n_merge} merge")
    print(f"    {nid}: {', '.join(parts)}")

# === Compute depths from parent chain ===
def compute_depth(node_id):
    n = nodes[node_id]
    if n['parent'] is None:
        n['depth'] = 0
        return 0
    if n['parent'] not in nodes:
        # Create missing parent
        ensure_node(n['parent'])
    d = compute_depth(n['parent']) + 1
    n['depth'] = d
    return d

for nid in list(nodes.keys()):
    compute_depth(nid)

# === Assemble output ===
node_list = sorted(nodes.values(), key=lambda n: (n['depth'], n['id']))

total_add = sum(n['additions'] for n in node_list)
total_del = sum(n['deletions'] for n in node_list)
merged_count = sum(len(n['prs']) for n in node_list if n['prs'])

output = {
    'meta': {
        'repo': 'tidepool-heavy-industries/tidepool',
        'generated': datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ'),
        'node_count': len(node_list),
        'pr_count': len(prs),
    },
    'stats': {
        'merged': len(prs),
        'closed': sum(1 for p in all_prs if p['state'] == 'CLOSED'),
        'open': sum(1 for p in all_prs if p['state'] == 'OPEN'),
        'total_additions': total_add,
        'total_deletions': total_del,
    },
    'nodes': node_list,
}

with open('graph-data.json', 'w', encoding='utf-8') as f:
    json.dump(output, f, indent=2, ensure_ascii=False)

print(f"Done. {len(node_list)} nodes, {len(prs)} PRs → graph-data.json")
print()

# Show tree
def print_tree(nid, indent=0):
    n = nodes[nid]
    merge = ''
    if n['prs']:
        ts = [p['mergedAt'] for p in n['prs'] if p.get('mergedAt')]
        if ts:
            merge = f"  [{ts[-1][5:16]}]"
    kids = sorted([c['id'] for c in nodes.values() if c['parent'] == nid])
    kid_str = f"  ({len(kids)})" if kids else ""
    print(f"{'  ' * indent}{n['label']}{kid_str}{merge}")
    for kid in kids:
        print_tree(kid, indent + 1)

print("Tree:")
print_tree('main')
