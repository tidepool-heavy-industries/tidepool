# Build snapshots for actor forks

Status: local feasibility demonstrated; production integration remains unimplemented.
This records the build-cache discussion alongside the parallel dogfood run. It does
not change the running harness.

## Agreed behavior

Inherit build resources from the **orchestration parent**, independently of which
actor supplied the conversation prefix. Seed the root once from a warm build, or
build there before substantial fan-out.

**A fork uses the parent's latest completed warm snapshot.** If the parent is
building, leave that build and its TUI running. Give each child a private writable
layer over the retained snapshot. An older cache does not change the child's
selected source commit; Cargo decides which artifacts need rebuilding.

Snapshot publication is separate from actor forking. Attempt publication after
owned work has finished and writers have quiesced. A busy filesystem leaves the
previous snapshot current; it does not fail the actor fork, cancel the build, or
terminate the TUI. Do not make an agent spend turns polling for publication.

Before the first warm snapshot exists, the correctness fallback is an empty private
build directory. Planning an initial warm build avoids an expensive cold fan-out.
Inherited snapshots remain available until a child can publish its own newer one.
No automatic build cancellation is part of this default.

Source-workspace extension: ordinary `unfold` should preferentially snapshot the
actual workspace, preserving dirty, untracked and ignored files plus timestamps.
A busy source uses the ordinary linked Git-worktree fallback automatically, with
a concise notice that working files were not inherited. This is separate from
the last-completed-build-snapshot fallback. The source/index and host-visible
checkout integration remain unimplemented.

Suggested agent guidance:

> Before unfolding implementation workers, finish any useful shared build and leave
> the build directory idle when practical. Children inherit your latest completed
> warm snapshot. If a build is still running, proceed with the previous snapshot;
> do not cancel useful work merely to fork.

At the next launch, the human and managed Astra planner should use the saved wave's
branches and build observations to choose the tree together. Put useful shared
builds before fan-out, retain implementation/validation owners with warm caches,
and parallelize independent obligations. Avoid adding serial build ceremonies to
every intermediate scaffold or treating a fixed role tree as the objective.

## What the local probes established

On this host's ext4 filesystem and Linux 6.12.63:

- `cp --reflink=always` is unsupported. Bubblewrap and kernel OverlayFS work without
  migrating the filesystem or using FUSE.
- A tiny Rust project built in a parent directory was `Fresh` in a child overlay.
  Changing the child's source rebuilt its binary without changing the parent's.
- Identical source bytes with a newer mtime caused Cargo to rebuild. Warm target
  inheritance alone does not guarantee unchanged workspace crates stay fresh in a
  new Git checkout.
- Remounting an overlay read-only failed with `EBUSY` while a writable descriptor
  was open. Closing it allowed freezing. A fresh writable parent view and a child
  view then diverged independently over the frozen snapshot; deletion state was
  preserved and snapshot writes failed with `EROFS`.
- Naively nesting merged overlay views failed at the third overlay generation.
  A flat list of frozen raw layers passed eight generations, preserving deletions
  and avoiding copying an untouched 1 MB file into any upper layer.

Private probe programs and logs are currently in
`target/build-snapshot-research/`; the Cargo probe results are in
`target/dogfood-launch-20260908/overlay-probe.json`. These are temporary local
evidence, not committed regression tests. The mount experiment ran inside one
private namespace with a trusted helper. It did **not** demonstrate a live native
TUI surviving rotation, cross-namespace child launch, or crash recovery.

## Owning implementation

Extend `BuildResourceLease` in `tidepool/src/actor_host.rs` and the mount boundary
in `tidepool-node/src/process_boundary.rs`. Today's writable “overlay” is a bind
mount of a separate empty actor build directory. Preserve those owners and their
cleanup obligations; do not add another actor scheduler or competing registry.

Represent a published snapshot as retained immutable layers. Each actor uses a
private upper/work pair over those layers. Publication must:

1. Exclude new writes through the owning execution boundary while settling existing
   writers. A completed Cargo process alone does not prove background descendants,
   other builds, or tests have stopped writing.
2. Freeze the current view. On `EBUSY`, keep the previous snapshot and leave normal
   execution usable; retry at a later suitable completion boundary.
3. Prepare a fresh writable parent view. Retain the old layer, publish its snapshot
   identity, and resume normal admission through a recoverable transition.
4. Retain all layers while any actor, mount, or uncertain process still depends on
   them. Parent retirement must not delete a descendant's lower layers.

Only the trusted mount owner receives mount capabilities. Workers must not receive
those capabilities or writable aliases to frozen backing directories. An overmount
does not retarget existing cwd/directory descriptors; that needs explicit handling
before this can operate safely around a persistent TUI.

Use flat lower-layer lists, not recursively nested merged mounts. Bound layer
growth and provide eventual consolidation: unlimited depth is not free. Ext4
OverlayFS copies up at file granularity, so modifying a large inherited file can
still copy that file. Importing an existing ordinary ext4 target also has a real
one-time cost and requires a stable source; a read-only alias does not freeze its
other writable aliases.

Reuse Cargo's validity checks. Never backdate arbitrary source files to manufacture
cache hits. Any future preservation of checkout metadata must establish matching
content and relevant metadata through the worktree owner, including conservative
handling of build-script inputs. Keep the actor-visible path and toolchain
selection stable where possible. Existing wrapper disabling must remain until a
cache daemon's mount-namespace behavior is separately solved.

## Remaining decisive checks

- A persistent native TUI remains usable across publication; a separate child
  namespace sees an isolated writable cache.
- Active builds, descendant writers, stale directory descriptors, and admission
  races leave the prior snapshot usable without killing or wedging the actor.
- Interrupted publication and parent retirement preserve dependent children and
  eventually reclaim unused layers.
- A representative repository build benefits across real managed checkouts while
  changed source, compiler options, and build-script inputs invalidate correctly.
- Recursive generations and consolidation preserve deletions and bounded storage.

Coordinate changes to execution admission and process lifetime with the active
interactive-applications work. The cache is an optimization: publication failure
must remain separate from hosted tool completion and actor liveness.

## Primary references

- [Kernel OverlayFS documentation](https://docs.kernel.org/filesystems/overlayfs.html):
  copy-up, whiteouts, lower-layer sharing, and restrictions on modifying backing
  layers while mounted.
- [Linux 6.12 filesystem definitions](https://github.com/torvalds/linux/blob/v6.12/include/linux/fs.h)
  and [OverlayFS setup](https://github.com/torvalds/linux/blob/v6.12/fs/overlayfs/super.c):
  filesystem stacking limit and its enforcement.
- [Bubblewrap interface](https://github.com/containers/bubblewrap/blob/main/bwrap.xml):
  overlay sources and private writable/work directories.
- [Cargo fingerprint documentation](https://doc.rust-lang.org/stable/nightly-rustc/cargo/core/compiler/fingerprint/index.html):
  artifact validity and timestamp-based freshness.
- [Cargo 1.93 file locking](https://github.com/rust-lang/cargo/blob/rust-1.93.0/src/cargo/util/flock.rs):
  advisory locking is insufficient to exclude arbitrary filesystem writers.
