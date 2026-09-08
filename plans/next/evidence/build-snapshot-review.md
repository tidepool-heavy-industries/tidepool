# Review: transparent, cache-ready unfolds

The storage direction remains appropriate for the current ext4 host: immutable
OverlayFS layers and private writable views avoid eager build-tree copies and can
preserve source file timestamps. The checked mount transition is useful substrate,
not yet proof of transparent ordinary unfolding. Native admission is staged in
the isolated Codex worktree; see [its checkpoint](native-workspace-admission.md).

## Consolidate around an owned workspace view

Represent one actor's source view, stable visible root, private Git state, build
view and separately owned nested mounts together at composition. Keep source and
build snapshot identities distinct: a busy parent can supply the older completed
build snapshot. Storage mechanics stay in the mount boundary; checkout identity
and Git operations stay with the worktree owner; execution admission stays native.
There should be no independent path conventions or layer-management workflow for
models to coordinate. Ordinary `unfold` remains the only normal surface.

## Consequential findings

1. **Source rotation must preserve the complete mount tree.** The checked
   current working implementation stages the replacement with explicitly owned
   nested mounts in a private helper namespace and publishes the tree in one
   graft. The production mount builder and focused test cover a live build FD,
   canonical `.shoal`, and rollback after failed assembly. Actual source allocation
   must supply this layout from its owner; it is not yet connected to unfolding.

2. **Namespace-aware Git is necessary but insufficient.** Manager lookup/list,
   monitor presence, submission and operation-marker inspection now use the owning
   Git client's filesystem view. The focused namespace test verifies a checkout
   whose `.git` exists only in its mounted view and private merge metadata hidden
   from the host. Unavailable filesystem access fails rather than reporting a
   missing checkout. Creation still contains host `canonicalize` calls, and actor
   composition does not yet bind each checkout to its namespace. Distinguish the
   registered checkout identity from its stable namespace-visible path once, at
   the worktree owner, rather than patching each caller with another path rewrite.

3. **Cache continuity needs metadata and path checks.** Empty replacement upper
   directories receive allocation-time metadata unless explicitly initialized.
   Rotation now copies visible root ownership, mode, xattrs and timestamps, with
   focused coverage. Initial child-view allocation still needs that invariant.
   Preserve source contents/timestamps and visible
   source/target paths; leave Cargo's validity checks intact. Acceptance must
   include a build script and a changed local crate, not just an unchanged tiny
   build. Incremental-cache hard links can trigger file copy-up; measure changed
   build storage as well as cheap fork creation.

4. **Pause native mutations, not hosted coordination.** `unfold` can execute
   inside the long-lived hosted Haskell/tool call. A permit covering that whole
   call would make its own snapshot impossible. Native exec, patching and owning
   filesystem mutations need admission exclusion. Agent-turn limits do not
   establish this. Shell exit does not prove descendant exit; uncertain ownership
   must preserve the prior build snapshot and use the authorized source fallback.
   Do not grow a second general process supervisor to guess quiescence.

5. **Retain generations, not just actor directories.** Build leases now hold
   immutable layer dependencies through shared resource custody. Launch selects
   the orchestration creator's latest published snapshot; inherited snapshots
   remain available to subsequent children. Uncertain process custody preserves
   all backing dependencies on disk. The resource owner records current and
   pending view recipes, but restart reconciliation and confirmed reclamation are
   unfinished. Automatic publication is not enabled without native admission.
   Bound both lower-layer depth and retained old mount
   trees; a flat lower list alone does not retire covered mounts or stale cwd
   references. Native cwd refresh belongs at the execution boundary.

   Filesystem custody must also outlive worker execution. `MountNamespace` retains
   namespace/root descriptors but currently requires a live original process for
   host commands. Finished checkouts must remain inspectable through their retained
   view. Separate that authority from native process liveness within the resource
   owner; do not simply remove the current check without replacing its contract.

6. **Make root import and root Git ownership explicit.** The original ext4
   checkout cannot be treated as an immutable lower layer while ordinary host
   writes continue. Import a stable seed without silently divorcing the source
   repository's HEAD/index from its working files. Keep canonical `.shoal`
   separately authoritative. Confirm these semantics in the first integrated
   parent/child path, not after generalized snapshot machinery is finished.

## Next decisive implementation checkpoint

Implement one real ordinary unfold that joins source, Git state, build inheritance
and native admission. Verify staged/unstaged/untracked files, timestamps, nested
mounts, independent child Git changes, unchanged-build reuse, and correct rebuilds
for changed inputs. Include a busy parent: it remains usable and the child receives
the documented fallback. This is the next priority before broader consolidation,
recovery and telemetry work; those remain required for full completion.

The composed source/build fixture in
`tidepool-worktree/tests/source_build_fork.rs` now proves the filesystem/Git/Cargo
part together. It rotates a built parent's source and nested build views, starts
an independent child over the frozen layers, and preserves staged versus
unstaged changes, deletions, ignored/untracked files, and source file mtimes.
Cargo reports every inherited compiler artifact fresh. Changing a build-script
input and then local Rust source each rebuilds the child with the expected new
output; the parent's executable, index and HEAD remain independent. No source or
target file tree is copied on the child path: the fixture copies only the Git
index and the child's `.git` pointer. This does not yet measure allocated bytes
or prove resource retirement.

Check: `cargo test -p tidepool-worktree --test source_build_fork -- --nocapture`
passed. This is a provider-free composition check using
production mount and Git primitives, not native admission or managed-unfold
acceptance. Child Git preparation now uses
`WorktreeManager::prepare_inherited_source`, sharing the existing provisional
allocation path. It preserves staged/unstaged changes, intent-to-add and index
flags, and expands a copied split index without changing the parent's index
bytes. The prepared checkout has independent HEAD/branch/index and no freshly
checked-out files. It remains provisional until a source view is installed;
normal lookup refuses a handle to present but unfinished storage. The fixture
still installs the child's source view manually. Host admission, source-view
registration/finalization and automatic fallback remain unwired.

After that owner change, the combined source/build test and all 22 worktree-core
checks passed. Clippy on both changed test targets, formatting and whitespace
checks passed. Serialized receipts keep their existing format: preparation uses
the existing provisional status rather than recording a completed checkout.

The source integration entry is `ActorForkWorkspaceAdmission::admit`, called by
`try_start_child` in `tidepool-actor/src/resident_actor.rs` before the child is allocated and
before `LocalResidentDeployment::PolicyInstalled`. Its current handler delegates
to `WorktreeManager::create_for_actor_path`, which resolves a clean commit or a
synthetic dirty commit. Capturing source only in the later launch event cannot
preserve the correct index/HEAD transaction. Move capture into admission and hand
the resulting retained workspace view to launch; preserve the separate creator
build snapshot selection. Do not reuse the synthetic-commit path for successful
live source inheritance.

Focused verification of the inspection changes: the namespace Git integration
test passed, including manager lookup/list/submission against mounted-only state;
three lost-worktree cases and the in-progress rebase refusal passed in
`worktree_core`. `cargo clippy -p tidepool-node --lib -- -D warnings` and
`cargo check -p tidepool --bin shoal` passed. These are boundary checks, not
managed-unfold acceptance.

The build-resource owner has three focused tests, run through
`just test-lib tidepool 'test(actor_host::build_resource::tests)'`. A real mounted
parent publishes a generation, remains writable while a busy publication preserves
the old snapshot, and supplies an independently writable child. Parent lease loss
and uncertain child custody retain the inherited layer. Preparation failures remain
retryable; a confirmed mount with a failed metadata write retries recording without
another rotation. Only an uncertain mount outcome requires mount reconciliation.
This exercises filesystem artifacts, not Cargo reuse or native admission.
The mount owner now immediately reconciles an uncertain helper result using
`statx` mount identity and the kernel's OverlayFS options in the same namespace.
It recognizes the intended writable replacement or restores only the original
mount. A different recipe or unavailable observation remains unconfirmed. Three
real-mount recovery tests cover lost receipts after publication, interruption after
freeze, and refusal to modify an unexpected replacement. Paths include spaces,
commas, colons and backslashes; this also exposed and fixed upper/work argument
escaping. These tests do not implement recovery across a host restart or retain
a retryable mount witness when immediate observation is unavailable.
Shoal compilation passed. Strict application Clippy remains blocked by existing
warnings in hosted retirement, launch custody and prompt hashing (and a dependency
warning in rollout usage without `--no-deps`); the new excessive-argument warning
was fixed by grouping the two independent inheritance inputs.

## Alternative substrate

A dedicated filesystem with native subvolume snapshots could remove much of the
live OverlayFS rotation machinery. That would change host storage provisioning;
it is an alternative to discuss, not an authorized migration or a hidden fallback.
On the existing ext4 setup, keep OverlayFS and simplify the ownership model above.

References:
- [OverlayFS semantics](https://docs.kernel.org/filesystems/overlayfs.html): directory
  metadata, copy-up on writes/metadata changes/hard links, and layer restrictions.
- [Btrfs subvolumes](https://btrfs.readthedocs.io/en/latest/btrfs-subvolume.html): native
  subvolume snapshots as a possible different storage substrate.
