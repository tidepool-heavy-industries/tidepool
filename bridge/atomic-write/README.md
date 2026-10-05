# tidepool-atomic-write

The one same-directory atomic write-then-rename helper, shared by every
durable on-disk store in the workspace — the worktree registry, the agent
binding table, the self-harness checkpoint, the run lease, the toolchain
stamp, and a session's compiled-module cache. Each of those used to write
the same way by hand: a temp file in the *same* directory as the target (so
the final rename stays on one filesystem and is atomic), then a rename over
the target, so a reader — this process's own next boot, or a concurrent one
— can never observe a torn write. This crate is that one mechanism.

## What's here

Publication and directory durability primitives (`src/lib.rs`):

- `write_durable(path, bytes)` — file fsync + strict parent-directory
  fsync, so the rename itself survives a crash. Use for state a restart must
  be able to trust: registry rows, leases, checkpoints, toolchain stamps.
- `write_best_effort(path, bytes)` — no fsync at all; only the rename's
  atomicity (never a torn read) is kept. Use for regenerable caches, where a
  lost write on a crash is just a future cache miss, not data loss.
- `stage_durable(path, bytes)` — write and fsync a same-directory temporary
  file without changing the target. Its `StagedDurableWrite::publish` reports
  `BeforeRename` separately from `PublishedDurabilityUnconfirmed`; the latter
  includes a `PublishedWrite` receipt whose `confirm_durability` retries only
  the parent-directory sync. Callers must serialize writes to that target
  through confirmation. Existing `write_durable` callers keep their API.

- `write_durable_new(path, bytes)` — durable exclusive creation. It reports
  whether this caller published the file or another publisher already owns its
  name. Both outcomes confirm the parent-directory sync; an error can follow a
  visible publication.
- `DirectoryAnchor::open_existing(root)` — open a stable domain directory whose
  own parent link the owner has already durably established. It never creates
  the root. `anchor.create_dir_all(relative)` rejects absolute and parent paths,
  creates the hierarchy, then syncs the full leaf-to-anchor chain, including
  existing directories on every retry. It never opens ancestors above the
  anchor. A failed creation must be retried from the same stable boundary.

Publication uses `tempfile::NamedTempFile` for unique same-directory temporary
files. `WriteError` names the path touched by the failed operation, including
parent-directory open or sync after publication. Directory errors are never
suppressed. The anchor scopes durability; it does not confer filesystem access
or replace the owner's concurrency and symlink-target preconditions.

## How to change it

This crate is intentionally tiny and dependency-light (`tempfile` only).
Keep durability and publication ordering in this owner. Callers select the
stable storage domain and serialize mutations; they must not infer an anchor
from whichever ancestor happens to exist after an uncertain creation.
