# Managed root workspaces and reusable artifact baselines

## Accepted direction

Every root starts on managed source, with the same immutable-lower/private-upper
semantics as its children. Commits stay on a run-owned integration branch;
explicit merge/export is required to change the host checkout. External host edits
are not live inputs to the run. User approved 2026-09-25.

Config.toml controls source selection and artifact import. Import existing build
artifacts by default; missing directories start empty. Source, build artifacts,
helpers, toolchains, shared caches and scratch have distinct owners/mounts.
Preserve root and child fork snapshots including intended uncommitted source.
No mutable-host lowerdir or hardlinks to mutable files. Do not restart live hosts
or compiler daemons to install this change.

## Contracts and implementation sequence

1. **Policy**: extend the existing launch/config owner with typed source and
   artifact policies. Source defaults: tracked files plus local changes and
   ordinary untracked source; ignored source needs explicit inclusion. Preserve
   symlinks as source entries without traversing unrelated targets. Handle Git
   submodules explicitly. Artifact default is `target`, import-existing; configure
   Cabal outputs in this project's config. Artifact paths are excluded from source
   automatically. Reject path escape, reserved mounts, ambiguous overlapping
   mounts and exclusions containing tracked source. Define migration from existing
   launch.source_exclude without two competing policies.
2. **Import/persistence**: extend existing OverlayResource storage/publication.
   Persist immutable artifact snapshots across runs, under workspace identity and
   normalized mount/import policy. Each run owns a private writable layer. Reuse
   only completed compatible published snapshots; do not silently merge concurrent
   run uppers. Initial host artifact import is a seed, not a promise to mirror all
   future host builds. Explicit refresh creates a new generation; old users retain
   their immutable generations. Missing source starts empty. Record provenance.
3. **Root integration**: allocate private Git/worktree through existing owner,
   capture selected host source once before model admission, mount root on managed
   source and configured artifact overlays. Stable actor-visible paths across root
   and descendants. Existing layer publication drives subsequent forks, with no
   host-tree copy/hash on ordinary managed forks. Keep helpers separate. Record
   host baseline and run branch for explicit integration.
4. **Lifecycle and diagnostics**: source imports have progress, bounded resource
   use and cancellation through existing process ownership. Partial staging is
   never published. Report copy versus reflink rather than silently describing a
   full copy as a fast clone. Quiesce relevant managed writers before freezing a
   layer; copying a live writable host directory requires mutation detection and
   refusal/retry, not a claim of atomic snapshot. Cleanup retains live layers.
5. **Documentation/acceptance**: config reference and migration, initial import
   cost, external edit isolation, explicit integration, refresh/empty-start, path
   portability and invalidation. Verify real root->child->grandchild isolation,
   late parent writes, rename/delete, failed/cancelled import, changed policy,
   concurrent runs and warm artifact reuse. Use small fixtures, not this live
   checkout's large caches.

## Artifact reuse limits

Cargo can reuse compatible artifacts; imported target trees are not universally
relocatable. Dep-info has absolute paths by default; build scripts can retain
paths in OUT_DIR/output. Toolchain, flags, features, target and actor-visible path
changes can invalidate reuse. Preserve a stable in-sandbox path; let Cargo decide
freshness. Never rewrite fingerprints or claim cache hits merely from copied files.
Cabal/package-manager artifacts have their own validity checks. Add a small
relocation/build-script fixture and report measured warm reuse versus rebuilding.

References: https://doc.rust-lang.org/cargo/reference/build-cache.html and
https://doc.rust-lang.org/cargo/reference/build-scripts.html .

## Parallel ownership

- managed-workspace-policy: config types/validation, selection policy, config docs.
- managed-artifact-import: existing overlay resource owner, durable artifact
  generations/import receipts, cancellation boundary and focused tests.
- supervisor: managed root branch/bootstrap wiring, policy/source integration,
  process publication invariants, acceptance and documentation integration.

Agents agree concrete Rust types before wiring consumers. Work in isolated
worktrees. Keep these changes separate from active wave12 and Astra processes.
The helper declaration-lookup repair continues independently first.
