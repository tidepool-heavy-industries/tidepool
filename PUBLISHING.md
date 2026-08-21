# Publishing to crates.io

**Verified as of commit `e49b8cb0` (2026-08-20)** against `cargo metadata`.
Re-run the recipe below after adding a crate or changing inter-crate
dependencies — don't hand-edit this list and let it drift.

## Publish Order

Crates must be published in dependency order. Wait for each crate to appear on crates.io before publishing the next.

```
 1.  tidepool-bignum
 2.  tidepool-repr
 3.  tidepool-eval
 4.  tidepool-bridge
 5.  tidepool-effect
 6.  tidepool-heap
 7.  tidepool-codegen
 8.  tidepool-atomic-write
 9.  tidepool-worktree
10.  tidepool-agent
11.  tidepool-bridge-derive
12.  tidepool-bridge-effects
13.  tidepool-extract-cmd
14.  tidepool-runtime
15.  tidepool-mcp
16.  tidepool-handlers
17.  tidepool-repl (binary)
18.  tidepool-optimize
19.  tidepool-protocol
20.  tidepool-macro
21.  tidepool-harness
22.  tidepool-web (binary)
23.  tidepool (binary)
24.  tidepool-lsp (binary)
```

This order is topologically sorted from the workspace dependency graph
(`cargo metadata`, normal deps only, workspace members only) — see
"Regenerating this order" below for the exact recipe. Any order that keeps
each crate after all its own workspace dependencies is valid; this is one
such order, not the only one.

`tidepool-testing` and the two example crates (`tidepool-guess`,
`tidepool-tide`) have `publish = false` — crates.io ignores them. Everything
else in the workspace publishes, including the four binaries in the list
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
cargo publish --dry-run -p tidepool-repr
cargo publish --dry-run -p tidepool-eval
# ... etc
```

## Publish

```bash
cargo publish -p tidepool-repr
# wait for it to appear on crates.io
cargo publish -p tidepool-eval
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
