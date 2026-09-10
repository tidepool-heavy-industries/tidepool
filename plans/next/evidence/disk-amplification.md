# Workspace storage and next-run verification

`scripts/dev-shell.sh` selects committed Git flake inputs while retaining the
command's checkout and Cargo target. `just` uses this owner. An inherited
`IN_NIX_SHELL` marker cannot bypass selection. Do not import mounted workspaces
with `nix develop path:.`; that included warm build artifacts in store sources.

Source/build publication shares flat immutable layers without automatic full-tree
flattening or arbitrary depth/rotation caps. Actual kernel rejection preserves the
previous completed snapshot. Inside native write admission, an unchanged empty
upper can reuse that snapshot. Initial import and file-level copy-up still cost
space; this mechanism is not a disk quota.

Exact native and hosted cleanup permit worktree retirement: preserve working
files and the existing Git index/HEAD, detach owned mount views, then release
storage. Descendant leases retain their inherited layers. Lost or unconfirmed
process custody retains backing storage even when an in-memory handle disappears.
Degraded receipts go to the actor's supervisor, not unconditionally to root.

For old runs, `shoal cleanup --run-root RUN_ROOT` reports build storage.
`--apply` removes only storage whose process/mount inspection permits reclamation.
The existing host lifetime lock excludes live hosts and restart during cleanup;
uninspectable or descriptor-pinned namespaces retain storage. Source trees and
Git state are outside this command's deletion scope. This is explicit maintenance,
not automatic crash recovery. Custom launcher dependencies must also stay outside
disposable targets; Shoal retains its selected host and extractor executables in
the run directory, and packaged Codex uses its existing Nix retention mechanism.

## Focused evidence

- Composed production workspace-admission fixture: original root, warm child,
  recursive dirty source/index inheritance, busy fallback, uncertain publication,
  caller loss, and retirement with a warm descendant. Measured build backing:
  parent 8,036,352 bytes; new child 20,480 bytes.
- Overlay owner: 40 generations preserve whiteouts and the original artifact
  inode/blocks without flattening; unchanged-root metadata checks, busy writers,
  parent/child reclamation and lost descendant custody are exercised.
- Offline cleanup: live host exclusion, open descriptor retention and symlink
  rejection. No old live swarm storage was removed by the new command.
- Existing scoped supervisor/custody tests and host-incarnation tests pass.
- Selected runner remains executable after deleting its original target.
- Dev-shell checks cover pinned selection, retained cwd, false shell markers,
  dirty toolchain inputs, explicit overrides and rejected path imports.

These are model-free mechanism checks. The next live run must select a newly
built main runner; the existing swarm's frozen executable does not acquire them.
