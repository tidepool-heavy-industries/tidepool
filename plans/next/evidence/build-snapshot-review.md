# Review: transparent, cache-ready unfolds

Historical checkpoint. For current production composition, removed restart
machinery and acceptance status, read
[workspace fork implementation](workspace-fork-implementation.md).


Review baseline: Tidepool 43ddd89f on work/build-snapshots; native Codex
06d99357be on work/build-snapshot-admission. The runner still pins d760c5cb8c
and does not enable CODEX_WORKSPACE_SNAPSHOTS.

The mechanism is feasible, but the delivery has drifted: substantial support code
landed before ordinary unfold inherited source. Keep the useful primitives and
finish their production composition. Do not rebuild the substrate.

The operator has settled two important choices:

- Root keeps editing the original checkout, with its original HEAD/index and
  canonical .shoal. Its direct children require source import; deeper managed
  children can use CoW layers.
- A Shoal host crash ends the wave. Recover subsequent work from Git state.
  Live Haskell, namespace and snapshot-transaction reconstruction are not required.

The [revised design](../build-snapshot-implementation.md) and
[ordered tasks](../workspace-fork-tasks.md) supersede the earlier restart-oriented
implementation sequence.

## What is actually working

| Boundary | Evidence | Missing from ordinary unfold |
| --- | --- | --- |
| Mounts | Flat frozen layers, private writable children, parent continuation, busy-FD refusal, nested mounts, metadata-preserving rotation | Source allocation and bounded layer/mount growth |
| Git and Cargo | Private HEAD/index, dirty/untracked/ignored source, mtimes, immediate-parent Git preparation; unchanged artifacts fresh, changed inputs rebuild | Production source capture, root import and automatic fallback |
| Native writes | Local mutation gate, descendant accounting, replay-safe begin/finish, unsupported-executor refusal | Matched runner activation and owning-TUI acceptance |
| Actor composition | Owned preparation reaches bootstrap; launch enters a retained namespace; admission retains creator build selection | Source view in that preparation, rather than legacy Git materialization |
| Retention | Dependencies survive parent lease loss; unknown custody retains storage | Bounded growth and truthful release of resources no longer referenced |

This review reran:

~~~sh
cargo test -p tidepool-worktree --test source_build_fork -- --nocapture
~~~

One test passed, zero ignored; test body 2.72 seconds. It exercises production
filesystem/Git primitives and Cargo, but manually composes the parent source
layers. It does not prove native admission or managed unfolding. Earlier checks
remain in [native evidence](native-workspace-admission.md) and
[filesystem evidence](build-snapshots.md); they were not all rerun here.

## Findings that change implementation

1. **Publication coordination is at the wrong level.**
   actor_host/overlay_resource/native_publication.rs owns a sequence per resource,
   but Codex has one gate and sequence space per process. A second independent
   source publisher would conflict with the build publisher. Put one transaction
   on the existing workspace owner; keep layer mechanics on OverlayResourceLease.
   An older build snapshot is useful cache input; an older source snapshot is not
   the current working tree.

2. **Source capture cannot wait for deferred child startup.**
   ActorForkWorkspaceAdmission still calls AuthorizedForkWorkspace::materialize,
   which uses the ordinary Git allocator. ResidentKernelBehavior::start marks an
   exact-context fork ready before initialize runs; actual startup waits for the
   completed hosted call. Capture and finalize the child's view during admission.
   Use checkout identity for filesystem staging; bind actor identity later through
   the existing custody installer. Launch must enter that same view, not remount
   the same writable upper/work directories.

3. **The old allocator is not the required fallback.**
   resolve_dirty_or_clean rejects dirty source or synthesizes a dirty commit.
   Busy-source fallback must instead allocate at the selected parent's exact
   current committed HEAD, retain the independently selected warm build and report
   omitted working-file changes. Shipped projectHead/boundHead/snapshotDirty
   guidance still describes old behavior and must change with implementation.

4. **Source and cache ownership can differ.**
   Source authorization precedes capture. Gate the selected source's actual owner;
   the orchestration creator supplies the cache. If they differ, take the creator's
   last completed cache without acquiring its publication gate, then capture the
   source. Do not introduce two-actor lock ordering or silently use the creator's
   Git state. Host-side checkout mutations also need exclusion; native descendant
   accounting alone does not cover worktree effects.

5. **Restart requirements created avoidable machinery.**
   Keep in-wave mount reconciliation and replay-safe native finish. Remove
   restart-only loading/serialization of pending operations once its production
   replacement is connected. Retain simple resource identity/dependency records
   where needed for custody or inspection. A new wave starts from Git, not old
   overlay receipts. Existing staged record formats must not be silently adopted
   as a different format.

6. **Mounted-only source and ordinary Git files need distinct treatment.**
   mount_worktree currently marks every launched worker Mounted. Registry reopen
   then refuses even ordinary host-backed checkouts until a namespace is attached.
   Runtime namespace routing is useful for both; durable dependence on an overlay
   is only appropriate for source that actually requires one. Do not make ordinary
   Git recovery depend on reviving a dead workspace.

7. **Retention is not reclamation.**
   process_may_exist monotonically fences resource deletion; the current launch
   path cannot turn tmux disappearance into exact cleanup. Keep that safeguard.
   Workspace/mount references, active work and child lower-layer references all
   retain storage. Bound publication growth and reclaim only unreferenced storage
   whose custody is confirmed. Never make a successful actor response mean a
   filesystem is safe to delete, or expand this task into all of application A0-A8.

## Scope discipline

The next code deliverable must traverse real admission, child view preparation
and launch, with a dirty original root and a busy-source case. Extend it to a
managed grandchild before adding further abstractions. Root import must exclude
canonical .shoal, common Git administration and designated build resources;
copying a large ignored target would defeat the purpose.

Keep normal Codex TUIs, the existing process and Git owners, one Haskell unfold
surface, and Linux-only filesystem mechanics. No persistent snapshot daemon,
automatic revival after host death, host-filesystem migration or parallel registry
is needed. A host crash may lose uncommitted child work; committed branches remain
the recovery boundary, and uncertain storage is retained.
