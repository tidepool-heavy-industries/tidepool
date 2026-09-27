# Wave19 storage exhaustion and recovery

Investigation: 2026-09-27. Run `c4bd1f22-bf26-4a27-9f12-3ffc7cc29be2`.
The operator subsequently deleted `~/.cache`; free space rose to 509 GiB.
The observations below distinguish evidence retained before deletion from source
inspection. Cached journals and managed worktrees are no longer available for
repeat inspection. Repository run logs and provider rollouts remain.

## Failure sequence

- 09:55:18 UTC: inbox append and helper snapshot operations report ENOSPC.
- 09:55:26: actor65 cannot create a temporary generated Haskell source file.
- 09:55:45: a compiler request reports that the daemon crashed mid-request.
  The process death mechanism is not established; this is not evidence of OOM.
- 09:56:12: host generation1 exits after failure reporting encounters an
  already-exited actor17.
- Recovery generations2–8 fail root adoption. A separate root-selection defect
  picks a parentless operator actor with root privileges instead of the hosted
  model application, then rejects its absent application state.

The filesystem was ext4, effectively full, with only 24% of inodes used.
A small `cp --reflink=always` probe returned `Operation not supported`.

## Why isolation did not bound disk consumption

1. **Import selection was too broad.** `OverlaySource::import_source` copies
   every top-level entry except explicit exclusions. `source_exclusions` omits
   Git metadata, Exomonad state, configured exclusions and eligible cache-tagged
   directories. It does not select ordinary source using Git ignore rules.
   The supplied disk inventory shows a 118 GiB imported base containing 74 GiB
   of `.claude/worktrees`.
2. **Copy strategy was optimistic.** Import uses `cp --archive --reflink=auto`.
   On this filesystem that permits physical copying. Bubblewrap provides the
   mount boundary; it does not make the initial baseline a reflink. Sharing
   descendant overlay layers cannot undo an oversized physical baseline.
3. **Admission did not budget the copy.** The import can consume the space
   needed for journals, generated sources, and failure reporting. Concurrent
   admissions must also be considered: separate free-space checks are not a
   reservation, and external disk consumers can still exhaust space.
4. **Retention needs explicit accounting.** Immutable layers remain while
   referenced; cleanup cannot safely discard dirty work merely because a run
   stopped. Compaction also copies a merged view after 32 layers. Wave19 has
   three import events and five reuse events, but no observed compaction event;
   compaction is a potential amplification path, not the demonstrated cause.
5. **Durability classes are mixed.** Managed checkouts and run journals lived
   under `.cache` alongside regenerable artifacts. Deleting that directory
   therefore removes recovery evidence and potentially uncommitted work.
   Committed harness candidate `4d722a5` still resolves in the surviving Git
   repository; this does not prove that every dirty child edit survived.

The large Nix store, download directory and compiler cache in the supplied
inventory are additional consumers. Their sizes alone do not establish that
Exomonad created them or that their contents are safe to delete.

## Changes and remaining work

- In progress: import tracked files and ordinary untracked source, preserve
  submodule content and local modifications, and require explicit artifact
  imports for ignored trees. Inventory and drift checks must use the same
  selection as the copy.
- In progress: report selected size and copy strategy; enforce a configurable
  import cap and free-space reserve before copying. Estimates must account for
  sparse files and hardlink handling. Headroom is mitigation, not a guarantee
  against concurrent unrelated writes.
- Required follow-up: classify durable run state separately from disposable
  caches, and expose reclaimable versus live/dirty retained resources before
  deleting anything. No blanket cleanup is part of this investigation.
- Recovery patch: identify the logical actor owning the hosted application
  before choosing its newest incarnation. Never revive an older incarnation
  when its successor is incomplete or retired. Eight focused recovery tests
  passed, including operator ambiguity and incomplete-successor regressions.

## Verification retained

`just test-lib tidepool 'test(root_recovery) | test(recovery_preserves_root)'`
with the existing matched extractor and worker selected through supported
environment variables: 8 passed, 524 skipped. A first attempt stopped before
compilation because cache deletion removed the Cabal package index; the
successful invocation reused the already-built worker. No shared daemon was
restarted.
