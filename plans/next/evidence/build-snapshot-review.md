# Review: transparent, cache-ready unfolds

The storage direction remains appropriate for the current ext4 host: immutable
OverlayFS layers and private writable views avoid eager build-tree copies and can
preserve source file timestamps. The checked mount transition is useful substrate,
not yet proof of transparent ordinary unfolding. No native-admission changes have
been made in the isolated Codex worktree.

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

2. **Namespace-aware Git is necessary but insufficient.** Worktree registry
   presence checks instantiate a fresh `GitCli`; submission checks `.git` directly
   on the host; inspection and creation contain host `exists`/`canonicalize` calls.
   Those paths must use the same owned view as the native actor. Distinguish the
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

5. **Retain generations, not just actor directories.** The current lease deletes
   an exclusively owned actor directory, which cannot be used unchanged once a
   descendant retains layers inside it. Retention must account for layer users
   and uncertain cleanup. Bound both lower-layer depth and retained old mount
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
