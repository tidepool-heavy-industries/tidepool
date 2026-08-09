# L1 receipt — worktree core (registry, clean creation, restart lookup, binding)

## What landed

- `tidepool-worktree/src/git.rs::inspect::dirty_summary` — two
  `git status --porcelain=v1 -z` reads (one default, one
  `--ignored=matching`), classified into staged/unstaged/untracked
  (sorted, deduped via `BTreeSet`) plus an ignored count, with a small
  rename/copy-aware parser (`R`/`C` entries carry an extra `orig_path`
  field in `-z` output that must be consumed or every later entry
  misaligns).
- `tidepool-worktree/src/registry.rs` — `WorktreeRegistry::open/put/get/
  list/mint_id`, fully implemented:
  - One JSON file per worktree id under `<root>/records/`, written via
    `tempfile::NamedTempFile::new_in` (same dir) → `sync_all` → `persist`
    (rename), plus a best-effort directory fsync.
  - `open` creates the root if absent, canonicalizes it, and refuses (see
    "Deviations" below) a root that resolves inside *any* git working
    tree via `inspect::work_tree` + `starts_with`.
  - `mint_id` combines wall-clock ms, pid, an in-process `AtomicU64`
    counter, and a `RandomState`-derived random `u64`, with a
    collision-check-and-retry loop against the records directory — not
    clock-alone.
  - `list` never fails on one bad entry; liveness (`present`) is
    re-derived per row via `inspect::work_tree`, never stored.
  - Added `WorktreeReceipt.status: WorktreeRecordStatus` (`Provisional` |
    `Finalized`) — additive field, since `registry.rs` is L1's own module,
    not frozen scaffold.
- `tidepool-worktree/src/create.rs` — `WorktreeManager::create/lookup/
  list`, fully implemented:
  - `create` mints an id, resolves the seed commit per `WorktreeSource`
    (`CurrentRepository` → source `HEAD`; `Ref` → `rev-parse` in the
    source repo, no dirty/in-progress gate since it names already-
    committed content; `Worktree(id)` → `lookup` then that worktree's own
    `HEAD`), sanitizes `spec.label` into
    `tidepool/worktree/<sanitized>-<id>`, writes a **provisional**
    registry row, runs `git worktree add -q -b <branch> <cwd> <seed>`,
    then writes the **finalized** row. Provisional-before-materialize,
    finalize-after is the literal ordering in the code.
  - Dirty/in-progress checks apply to `CurrentRepository` and
    `Worktree(id)` sources (both read live working-tree state); `Ref`
    does not (PRD: "current repository or another worktree defaults to
    RequireClean" — a ref names a commit, not a working tree to copy).
    In-progress is checked unconditionally, before the dirty check, and
    refuses regardless of `dirty_policy`.
  - The `AllowDirtySnapshot` branch is wired exactly as instructed: it
    calls `snapshot::snapshot_source(&self.git, repo, id,
    temp_index_dir)` and uses `(snapshot_commit, Some(snapshot_ref))` as
    the seed. L2's `todo!()` still stands, so this path panics until L2
    lands — no test here exercises it (my acceptance scope was the
    refusal paths, per the dev spec's STEPS #7).
  - `lookup`: `Ok(None)` for never-registered, `Err(WorktreeLost)` for
    registered-but-gone (checked via `inspect::work_tree`, not a bare
    `Path::exists`, so a directory with a broken `.git` still reports
    lost). Works from on-disk state alone — no in-process cache.
- `tidepool-worktree/src/binding.rs` — `BindingTable`, fully implemented:
  - One JSON file per worktree id under `<root>`, holding that
    worktree's full lease history (every `bind` appends a row; `settle`
    edits the most recent `Active` row's state in place). Same
    temp-file/fsync/rename discipline as the registry. Loaded fully into
    memory at `open` so `current()` stays a cheap borrow returning
    `Option<&Binding>`.
  - `bind` → `WorktreeBusy { worktree, holder }` naming the current
    holder when an `Active` binding exists; otherwise appends and
    persists.
  - `settle` moves the active row to the given `BindingState`
    (`Terminal`/`Released`) and persists; a no-op `Ok(())` when there is
    no active binding (see "Deviations").
- `tidepool-worktree/tests/worktree_core.rs` — 15 tests, all against real
  `TestRepo`/`ScriptedWriter` repositories (no mocks). Test names, mapped
  to locked decisions:
  - `clean_creation_from_current_repository_leaves_source_untouched`,
    `clean_creation_from_a_ref`, `clean_creation_from_another_managed_worktree`
    — creation from all three sources; each verifies fingerprint + branch
    + HEAD + `dirty_summary` unchanged on the relevant source (root repo
    for the first two, the parent worktree for the third).
  - `dirty_source_refuses_by_default` — `SourceDirty`, and that a refused
    create leaves the registry empty (no provisional litter on a
    validation failure).
  - `source_mid_rebase_refuses` — a REAL conflicting rebase (two branches
    editing the same line, `rebase_onto` fails, leaves `rebase-merge` on
    disk), asserts `SourceOperationInProgress(Rebase)`.
  - `registry_and_lookup_survive_a_fresh_manager_over_the_same_root` — id
    created by one `WorktreeManager`, resolved by a second, independently
    constructed one over the same root.
  - `lookup_of_a_never_registered_id_is_ok_none` — the typo case.
  - `hand_deleted_worktree_is_lost_not_recreated` — `remove_dir_all` by
    hand, two consecutive `lookup`s both `Err(WorktreeLost)`, directory
    never reappears.
  - `list_never_fails_when_one_of_several_worktrees_is_lost` — one lost
    among two, `list` returns both rows with correct `present`.
  - `dirty_summary_classifies_staged_unstaged_untracked_and_counts_ignored`,
    `dirty_summary_is_empty_and_sorted_on_a_clean_repository` — direct
    unit coverage of the git.rs function, including two reads of the same
    state comparing equal.
  - `binding_refuses_second_agent_and_permits_rebind_after_settle`,
    `settling_a_released_binding_also_permits_rebind` — busy-refusal
    names the holder; rebind works after `Terminal` and after `Released`;
    durability across a fresh `BindingTable::open`.
  - `registry_open_refuses_a_root_inside_a_working_tree` — `#[should_panic]`,
    see deviation below.
- Left `tidepool-worktree/tests/scaffold_smoke.rs` in place: it exercises
  `amend`/`reset_hard`, which `worktree_core.rs` does not touch, so
  deleting it would drop coverage of those `ScriptedWriter` methods.

## Deviations from the literal spec text (and why)

1. **`WorktreeRegistry::open`'s inside-a-working-tree refusal is a
   `panic!`, not a typed `WorktreeError`.** The dev spec says "REFUSES a
   root that resolves inside a managed source working tree" but `open`
   takes only `root` — no source repository to compare against — so the
   check is necessarily the broader "is this inside *any* git working
   tree", not "is this inside *the* source". `error.rs` is frozen
   scaffold with no variant that fits (`NotARepository` means the
   opposite thing). Per the operational instructions ("if you genuinely
   need [a frozen file] changed, say so in your submit note rather than
   changing it"), I did not touch `error.rs`. **Flagging here:** a future
   pass may want `WorktreeError::InvalidRegistryRoot(PathBuf)` (or
   similar) so this refusal is catchable instead of a hard panic. Tested
   via `#[should_panic(expected = "resolves inside a git working tree")]`.
2. **`WorktreeSource::Worktree(id)` naming an unregistered id reuses
   `WorktreeError::WorktreeLost(id)`** rather than a distinct
   "not-registered" variant, for the same frozen-`error.rs` reason. Both
   mean "there is no usable worktree behind this id" from the caller's
   perspective; not tested directly (edge case — a caller passing a
   bogus id to `from_worktree`), but documented in `create.rs`.
3. **`BindingTable::new()` became `BindingTable::open(root) ->
   Result<Self, WorktreeError>`.** The module doc's own words — "Durable
   alongside the registry: a restart that forgot its bindings would
   happily hand a retained worktree to a second writer" — read as a
   requirement, not an option, so there is no in-memory-only
   constructor. `BindingTable` was not previously re-exported from
   `lib.rs`; I added it (and `WorktreeRecordStatus`) to the `pub use`
   list alongside the existing registry/create exports.
4. **`BindingTable::settle` on a worktree with no active binding is
   `Ok(())`, not an error.** No `WorktreeError` variant fits "nothing to
   settle", and treating a redundant settle as an error would make an
   idempotent cleanup path than needs try/catch to be safe to call twice.

## For root / other lanes

- The two frozen-file gaps above (`error.rs` needs a root-validation
  variant if the panic behavior is unwanted; `WorktreeSource::Worktree`
  not-found reuses `WorktreeLost`) are the only requests to change
  frozen scaffold. I did not make either change myself.
- `create.rs`'s `AllowDirtySnapshot` wiring calls
  `snapshot::snapshot_source(&self.git, repo, id, temp_index_dir)` with
  `temp_index_dir = <worktree_root>/.tidepool-snapshot-index/<id>`. L2:
  the seed used for `git worktree add` is `receipt.snapshot_commit`, and
  `WorktreeReceipt.snapshot_ref` is set from `receipt.snapshot_ref` — no
  other field threading needed on your end.
- `BindingTable` is standalone (constructed directly by callers, e.g.
  `BindingTable::open(some_root)`); it is not owned by or wired into
  `WorktreeManager`. That matches the HOLD line — nothing here reaches
  for a real agent handle or the coupled-spawn seam.

## VERIFY

- `cargo nextest run -p tidepool-worktree` — 15/15 passed (2 binaries:
  `worktree_core`, `scaffold_smoke`, plus the lib's own doctest-free unit
  binary).
- `cargo check --workspace` — clean.
- `cargo clippy -p tidepool-worktree --all-targets` — clean (one
  pre-existing `dead_code` warning on L3's `WorktreeMonitor.git` field,
  not touched by this lane).
- `cargo fmt --all -- --check` — clean.
