# Publishing to crates.io

**Verified 2026-09-14** against `cargo metadata`. Re-run the recipe below
after adding a crate or changing inter-crate dependencies.

## Publish Order

Crates must be published in dependency order. Wait for each crate to appear on crates.io before publishing the next.

```
 1.  tidepool-model-output
 2.  tidepool-model
 3.  tidepool-atomic-write
 4.  tidepool-repr
 5.  tidepool-heap
 6.  tidepool-bignum
 7.  tidepool-bridge
 8.  tidepool-bridge-derive
 9.  tidepool-bridge-effects
10.  tidepool-extract-cmd
11.  tidepool-tool
12.  tidepool-node
13.  tidepool-worktree
14.  tidepool-agent
15.  tidepool-effect
16.  tidepool-codegen
17.  tidepool-extract-report
18.  tidepool-toolchain
19.  tidepool-runtime
20.  tidepool-optimize
21.  tidepool-actor
22.  tidepool-mcp
23.  tidepool-handlers
24.  tidepool-harness
25.  tidepool-web (binary)
26.  tidepool (binary)
27.  tidepool-protocol
```

This order is topologically sorted from the workspace dependency graph
(`cargo metadata`, normal deps only, workspace members only) — see
"Regenerating this order" below for the exact recipe. Any order that keeps
each crate after all its own workspace dependencies is valid; this is one
such order, not the only one.

`tidepool-eval` (the standalone tree-walking reference interpreter) has
been removed from the workspace; it no longer appears above. `tidepool-repl`
is not currently a Cargo workspace member (its directory exists on disk but
carries no `Cargo.toml` workspace entry) despite still being named as an
active area in `CLAUDE.md`'s workspace map — flagged here rather than
resolved, since removing or restoring it is an architecture decision, not a
docs fix.

`tidepool-testing` has `publish = false` — crates.io ignores it. Everything
else in the workspace publishes, including the binary packages in the list
above (each depends on library crates that publish earlier).

## Known blocker: two publishable crates are path-only

`tidepool-atomic-write` and `tidepool-worktree` are declared in
`[workspace.dependencies]` (root `Cargo.toml`) with a `path` but **no
`version`**, unlike every other workspace-dependency entry (which carry
`version = "0.1.0"` alongside their `path`). `cargo publish` requires every
dependency — including path dependencies — to carry a version requirement;
as declared today, publishing `tidepool-worktree` (depends on
`tidepool-atomic-write`), `tidepool-agent` (depends on `tidepool-worktree`),
`tidepool-runtime`/`tidepool-harness`/`tidepool-web` (transitively depend on
both), or `tidepool-handlers`/`tidepool-repl`/`tidepool` will fail until a
`version = "0.1.0"` is added to both entries in `[workspace.dependencies]`.
This is a Cargo.toml change, out of scope for a docs-only pass — fix it
before attempting a real publish run.

## Regenerating this order

```bash
cargo metadata --format-version 1 > /tmp/meta.json
python3 - <<'EOF'
import json
d = json.load(open("/tmp/meta.json"))
members = set(d['workspace_members'])
pkgs = {p['id']: p for p in d['packages']}
nodes = {n['id']: n for n in d['resolve']['nodes']}

def normal_deps(pid):
    out = []
    for dep in nodes[pid]['deps']:
        if dep['pkg'] not in members:
            continue
        kinds = [dk.get('kind') for dk in dep.get('dep_kinds', [])]
        if None in kinds or 'normal' in kinds:
            out.append(dep['pkg'])
    return out

order, seen = [], {}
def visit(pid):
    if seen.get(pid) == 2: return
    seen[pid] = 1
    for d in normal_deps(pid):
        visit(d)
    seen[pid] = 2
    order.append(pid)

for pid in members:
    visit(pid)

for pid in order:
    p = pkgs[pid]
    if p.get('publish') == []:
        continue  # publish = false
    print(p['name'])
EOF
```

This prints a valid publish order (workspace members only, `publish = false`
crates excluded) from the live dependency graph — no hand-maintained list to
rot. Cross-check any path-only dependency at the same time:

```bash
# every workspace-dependency entry missing `version =` (path-only)
grep -E '^tidepool-[a-z-]+ = \{ path = ' Cargo.toml
```

## Dry Run

```bash
cargo publish --dry-run -p tidepool-model-output
cargo publish --dry-run -p tidepool-model
# ... etc
```

## Publish

```bash
cargo publish -p tidepool-model-output
# wait for it to appear on crates.io
cargo publish -p tidepool-model
# ... continue in order
```

## Cachix Binary Cache

Push Nix build artifacts to the `tidepool` Cachix cache for both Linux x86_64 and macOS aarch64.

### Setup

```bash
# Install cachix (if not present)
nix-env -iA cachix -f https://cachix.org/api/v1/install
# or: nix profile install nixpkgs#cachix

# Auth (needs token from https://app.cachix.org)
cachix authtoken <TOKEN>
```

### Push

```bash
# Build and push tidepool-extract
nix build .#tidepool-extract
cachix push tidepool $(nix build .#tidepool-extract --print-out-paths)

# Also push the dev shell closure
nix build .#devShells.$(nix eval --raw 'nixpkgs#system').default
cachix push tidepool $(nix build .#devShells.$(nix eval --raw 'nixpkgs#system').default --print-out-paths)
```

Run on both Linux x86_64 and macOS aarch64 to populate the cache for both architectures.
