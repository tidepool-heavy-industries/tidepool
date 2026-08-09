# L7 receipt — `worktreeHead`, a fresh-read HEAD lookup

Base commit: `ea9837e10b7bf073e5c29b192063d2715e9b8785` ("docs(worktree-lanes):
correct the poke-semantics note; record the cycle-shape change") — the tip of
this branch when this lane's implementation and tests were written and run.

## What landed

`WorktreeManager::worktree_head(&self, handle: &WorktreeHandle) ->
Result<GitOid, WorktreeError>` in `tidepool-worktree/src/create.rs`. One new
public method; no new type, no new export line needed (`WorktreeManager` was
already re-exported from the crate root). Five new acceptance tests in a new
file, `tidepool-worktree/tests/worktree_head.rs`.

## Where it lives, and why

On `WorktreeManager`, not `WorktreeHandle`. `WorktreeHandle` is documented as
"cheap to clone; a name plus its recorded facts, not an open handle to
anything" and carries no `GitCli` — so any method added directly to it could
only read fields already recorded on the handle, which by construction can
never be a fresh read. `WorktreeManager` is the only type in this crate that
owns a `GitCli` alongside worktree identity, and it already has exactly this
shape of method: `lookup(&self, id: &WorktreeId)` does a filesystem/registry
fresh-check, `worktree_head(&self, handle: &WorktreeHandle)` does a git fresh
read. Matching `lookup`'s receiver shape (an existing manager method taking
the id/handle) rather than inventing a free function keeps the one asymmetry
explicit: `lookup` is keyed by `WorktreeId` because it's finding a handle in
the first place; `worktree_head` is keyed by `WorktreeHandle` because the PRD's
authored signature (`worktreeHead :: WorktreeHandle -> M effs GitOid`) takes
one already in hand.

## Implementation

```rust
pub fn worktree_head(&self, handle: &WorktreeHandle) -> Result<GitOid, WorktreeError> {
    if !worktree_present(handle.cwd()) {
        return Err(WorktreeError::WorktreeLost(handle.id().clone()));
    }
    let out = self.git.try_run(handle.cwd(), &["rev-parse", "HEAD"])?;
    Ok(GitOid::from_raw(out.trimmed()))
}
```

- **Fresh read, no caching anywhere in the path.** `handle.cwd()` is a path
  (recorded once, never git truth itself); the method runs `git rev-parse
  HEAD` through `GitCli` on every call. Nothing on `WorktreeManager` or
  `WorktreeHandle` is consulted for the answer itself — the OID always comes
  from this invocation.
- **Detached HEAD:** `git rev-parse HEAD` resolves to the current commit
  identically whether `HEAD` is a symbolic ref or detached — no special-casing
  needed, unlike `read_branch` in `monitor.rs` (`symbolic-ref --short HEAD`),
  which legitimately returns `None` when detached. `worktree_head` never calls
  `read_branch`; it only ever resolves the commit.
- **Lost worktree:** reuses `registry::worktree_present`, the exact helper
  `lookup` uses, so a worktree removed from disk after the handle was obtained
  fails the same way everywhere in this crate:
  `WorktreeError::WorktreeLost(handle.id().clone())`. No new error variant.
- **Genuine git failure:** `GitCli::try_run` already lifts a nonzero exit or
  spawn failure to `WorktreeError::GitFailure`; nothing new needed here.
- **Storage failure:** `worktree_present` itself does no I/O that can fail
  loudly (it's `Path::exists` plus a `git` read, which itself degrades to
  `false` rather than propagating an I/O error) — there is no
  `StorageFailure`-producing step in this method's own body. (Registry/journal
  storage failures elsewhere in the crate are unaffected; this method never
  touches the registry.)

## Tests (`tests/worktree_head.rs`, 5 new)

Per the new swarm-wide receipt rule (fresh-read gates named individually, not
buried in an aggregate), each test below that exists specifically to catch one
failure mode gets its own named pass line, run in isolation with `cargo
nextest run -p tidepool-worktree -E 'test(=<name>)'`, on base commit
`ea9837e1`:

```
$ cargo nextest run -p tidepool-worktree -E 'test(=worktree_head_is_a_fresh_read_distinct_from_source_head)'
        PASS [   0.159s] (1/1) tidepool-worktree::worktree_head worktree_head_is_a_fresh_read_distinct_from_source_head
     Summary [   0.160s] 1 test run: 1 passed, 49 skipped

$ cargo nextest run -p tidepool-worktree -E 'test(=worktree_head_reflects_movement_the_monitor_never_reconciled)'
        PASS [   0.136s] (1/1) tidepool-worktree::worktree_head worktree_head_reflects_movement_the_monitor_never_reconciled
     Summary [   0.137s] 1 test run: 1 passed, 49 skipped

$ cargo nextest run -p tidepool-worktree -E 'test(=worktree_head_on_detached_head_returns_the_commit)'
        PASS [   0.158s] (1/1) tidepool-worktree::worktree_head worktree_head_on_detached_head_returns_the_commit
     Summary [   0.159s] 1 test run: 1 passed, 49 skipped

$ cargo nextest run -p tidepool-worktree -E 'test(=worktree_head_of_a_lost_worktree_fails_consistently_with_lookup)'
        PASS [   0.109s] (1/1) tidepool-worktree::worktree_head worktree_head_of_a_lost_worktree_fails_consistently_with_lookup
     Summary [   0.113s] 1 test run: 1 passed, 49 skipped
```

What each proves, and why it is split into its own test rather than folded
into another:

- **`worktree_head_is_a_fresh_read_distinct_from_source_head`** — the
  load-bearing gate. Creates a worktree, records `source_head`, then has
  `ScriptedWriter` commit *inside that worktree* (bypassing the manager
  entirely, exactly as a coding agent would). Asserts `worktree_head` returns
  the new commit while `handle.source_head()` still returns the original
  seed. An implementation that returns `handle.source_head()` under another
  name would pass every other test in this file and fail only this one — that
  is the specific failure mode this test exists to catch, so it gets its own
  pass line rather than sharing one with a broader assertion.
- **`worktree_head_reflects_movement_the_monitor_never_reconciled`** — the
  gap the verb exists to close. Registers a `WorktreeMonitor` against the
  worktree (establishing a baseline) but deliberately never calls
  `reconcile`. A commit is then made inside the worktree. Asserts
  `worktree_head` still sees the new commit, and separately asserts the
  journal for this worktree stayed empty (`since(0)` is `[]`) — proving the
  answer did not come from anything the monitor journalled either. This is
  the direct acceptance test for the PRD's stated reason `worktreeHead`
  exists: a resident spanning cycles closes the window between one cycle's
  handlers unregistering and the next's re-registering by comparing this
  verb against its own checkpoint, not by trusting the journal to have
  caught the movement.
- **`worktree_head_on_detached_head_returns_the_commit`** — commits, then
  explicitly detaches HEAD (`git checkout --detach HEAD`), confirms via
  `ScriptedWriter::current_branch() == None` that the tree is genuinely
  detached (not a false-positive pass), then asserts `worktree_head` still
  returns the commit rather than erroring for lack of a symbolic ref.
- **`worktree_head_of_a_lost_worktree_fails_consistently_with_lookup`** —
  removes the worktree directory by hand after the handle was obtained, then
  asserts both `manager.worktree_head(&handle)` and `manager.lookup(&id)`
  fail as `WorktreeError::WorktreeLost` naming the same id — proving the two
  entry points agree rather than inventing a second failure shape for the
  same condition.

One additional test, not itself a named gate against a specific failure mode
(so folded into the aggregate rather than given its own callout) but
supporting the "fresh read" claim further: `worktree_head_tracks_successive_commits`
makes two commits in sequence inside the worktree and asserts two separate
`worktree_head` calls return the two distinct tips in order — ruling out a
value memoized at the first call.

## Full-suite verification

```
$ cargo nextest run -p tidepool-worktree
     Summary [   1.385s] 50 tests run: 50 passed, 0 skipped
```

50 = the 45 pre-existing tests (unedited — no existing test file was touched)
plus the 5 new tests in `tests/worktree_head.rs`.

```
$ cargo check --workspace       # clean
$ cargo clippy -p tidepool-worktree --all-targets   # clean, no warnings
$ cargo fmt --all -- --check    # clean
```

## Scope discipline

- Only `tidepool-worktree/src/create.rs` (one new method) and
  `tidepool-worktree/tests/worktree_head.rs` (new file) were touched.
- `monitor.rs`, `snapshot.rs`, `registry.rs`, `binding.rs`, `git.rs`,
  `journal.rs`, `error.rs`, `id.rs` — untouched.
- No Haskell or effect-declaration file touched (`haskell/lib/Tidepool/*.hs`,
  `tidepool-mcp/src/effect_defs.rs` — none referenced or edited). The
  `wt-surface` lane owns generating the `worktreeHead` effect declaration
  against the Rust signature this receipt documents.
- No new `WorktreeError` variant. `worktree_head` fails only with variants
  that already existed before this lane: `WorktreeLost` and `GitFailure`
  (propagated through `GitCli::try_run`).
