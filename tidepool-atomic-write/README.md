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

Two functions, two durability tiers (`src/lib.rs`):

- `write_durable(path, bytes)` — file fsync + best-effort parent-directory
  fsync, so the rename itself survives a crash. Use for state a restart must
  be able to trust: registry rows, leases, checkpoints, toolchain stamps.
- `write_best_effort(path, bytes)` — no fsync at all; only the rename's
  atomicity (never a torn read) is kept. Use for regenerable caches, where a
  lost write on a crash is just a future cache miss, not data loss.

Both use `tempfile::NamedTempFile` for the temp file itself, which picks a
unique name per call — no caller needs to invent its own, and no caller can
collide with a sibling writer racing on the same target path. `WriteError`
names the path the failing step actually touched (the target's directory if
the temp file itself couldn't be created there, the target path for every
later step), because "the write failed" naming a directory when the target
file was never reached is a worse diagnostic than naming the real site.

Deliberately **not** here: `std::fs::hard_link`-based exclusive-claim writes
(an ordinary rename overwrites; a hard link fails loud when the target
already exists) — that's a different primitive for a different job, kept
with its one caller rather than folded in here.

## How to change it

This crate is intentionally tiny and dependency-light (`tempfile` only).
Adding a third durability tier or a new failure mode should stay in this
shape: one function per tier, `WriteError` naming the actual failing path.
If a caller needs something this crate doesn't provide (e.g. an exclusive
hard-link claim), that's a signal it wants a different primitive, not an
argument to widen this one — see "Deliberately not here" above.

## Where the rationale lives

The crate's own `src/lib.rs` module doc has the full six-caller history and
the fsync-tier reasoning; there is no separate plan document for this crate
specifically. `tidepool-runtime/CLAUDE.md`'s compile-cache section documents
a caller in detail (the compile cache).
