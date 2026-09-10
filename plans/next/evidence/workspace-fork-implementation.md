# Workspace fork implementation — 2026-09-08

Historical component checkpoint. Current supervised launch composition and its
acceptance gate are tracked in [supervised workspace activation](supervised-workspace-activation.md).

This implementation superseded the composition and restart gaps recorded in
[the earlier review](build-snapshot-review.md). Native source is
`06d99357becc4d870f5b5141ba7626daf68e819a`, pushed on
`work/build-snapshot-admission` and pinned by `flake.nix`/`flake.lock`.
No managed TUI or swarm has been launched for acceptance.

## Delivered behavior

Ordinary production admission prepares the whole source/build namespace before
returning the deferred actor preparation. Custody binds that prepared workspace;
native launch enters the same view. Root continues editing the original checkout.
Its children import source with metadata; managed descendants freeze source
layers. Private Git HEAD/index/branch and the canonical `.shoal` remain separate.
Root import excludes the conventional Cargo target only when its cache-directory
tag identifies it as a cache; ordinary source named `target` survives.

One workspace publication owner gates source and optional fresh build publication.
The selected source owner supplies working files; the orchestration creator supplies
its completed cache. Explicit refs bypass live capture. Busy/unavailable capture
and an in-progress Git operation use committed HEAD with an omission notice.
A failed original-source import reuses its unexposed provisional child for fallback.
Before/after inode, mode, size, mtime and ctime inventories detect changing imports;
this remains a coordinated copy, not an atomic snapshot against external editors.

Host Git commands share the GitCli owner's reentrant capture exclusion. Native
writer exclusion remains the native process-wide owner. Neither active builds nor
TUIs are killed to fork. A lost begin or finish retains the same sequence; the
existing fleet health task retries settlement. Known identity permits finish even
if namespace descriptor capture fails. Mount transitions retain their in-memory
reconciliation capability. Restart-only native and overlay transaction loaders
and serialized reconciliation capabilities were removed. `view.json` remains
resource/dependency metadata; old transaction files are not adopted or migrated.

Publication shares flat immutable lowers; no automatic flattening, eight-layer
threshold or 32-rotation policy remains. Actual kernel rejection leaves the
previous snapshot usable. An unchanged empty upper can reuse its completed
snapshot inside native write admission. Kernel-limit handling is not a disk quota.

Unsubmitted storage and confirmed retired storage are reclaimed. Retirement
preserves dirty working files and Git administrative state, detaches the exact
views, then releases storage only after descendant dependencies settle.
Used resources remain retained when exact process cleanup is unconfirmed. Ordinary
host-backed checkouts no longer acquire a durable overlay requirement merely
because launch routes Git through a namespace. A host crash ends the wave; committed
branches can seed fresh checkouts. Old TUIs, live Haskell and uncommitted overlays
are not reconstructed.

## Evidence

The real host admission fixture uses temporary Git repositories, real namespaces
and a bounded native-protocol backend. It checks root → child → grandchild,
private HEAD/index, staged versus working data, untracked/ignored files, mtimes,
parent independence, nested canonical/build mounts, inspection-only writes,
committed busy fallback, in-progress Git fallback, lost begin/finish, explicit
refs, distinct source/creator cache selection, native unavailability, and caller
cancellation while begin is pending. The owned operation finishes even when its
original awaiter is cancelled. Mounted registry receipts publish only after
their live view is installed, so concurrent listing can observe completed admission
without a transient missing-view error.

The same fixture builds a small Cargo crate. Unchanged child artifacts are fresh;
changed Rust source and changed build-script input rebuild. Readable backing
allocation was **8,036,352 bytes for root versus 20,480 bytes for the new child**,
counting hard-linked inodes once and excluding inaccessible kernel scratch.
These are fixture measurements, not a large-project performance claim.

Checks run:

- `just test-lib tidepool 'test(actor_host::workspace::tests) | test(actor_host::overlay_resource::tests) | test(actor_host::prompt_catalog) | test(shared_api_guide_example_handles_success_and_unavailable) | test(actor_host::custody_tests)'`: 22 passed.
- After simplifying custody, the workspace/overlay/custody selection: 17 passed.
- Extended host admission fixture with source/creator separation, explicit refs,
  unavailable native admission, cancellation and tagged-cache exclusion: passed.
  Its final run also covers concurrent listing during admission completion.
- Node `mount_namespace::overlay::recovery_tests`: 3 passed, including interrupted freeze and unexpected replacement.
- Worktree `source_build_fork`: passed; host Git capture exclusion test: passed.
- Native `writer_admission_tracks_detached_descendants`, explicitly running the ignored delegated-cgroup check: passed.
- Changed actor, agent, handler, node and worktree test targets compiled.

The standalone acceptance workspace definitions compile with `shoal check`;
no actors or providers were launched. The Shoal binary also built successfully.

The native CLI and code-mode host built from the exact pinned source using Rust
1.95 and the repository's hash-matched V8 archive/binding from the Nix store.
The raw upstream V8 download returned 404; the repository already specifies the
Codex-built artifact pair, which was used successfully. This was a local Cargo
build, not a completed Nix package build. Actual native TUI publication and its
interaction with hosted exact-context forking remain live acceptance gates.

## Prepared live acceptance

The standalone repository at
`target/workspace-fork-acceptance/project` has a committed tiny Cargo project and
`ACCEPTANCE.md`. It uses no Tidepool implementation task. The scenario permits at
most four actors: root, child, grandchild and a busy-fallback child. It checks
continued input, exact-context unfolding, Cargo reuse and disk growth. Use the
existing `shoal init` interface with a distinct session; no new launcher or TUI.
Obtain the operator's go-ahead before launching it.
