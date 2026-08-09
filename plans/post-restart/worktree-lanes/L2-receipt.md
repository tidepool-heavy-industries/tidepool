# L2 receipt — dirty-source snapshot

Lane L2 of the worktree-wave (PRD 19). Owns `tidepool-worktree/src/snapshot.rs`
and `tidepool-worktree/tests/dirty_snapshot.rs`.

## What landed

- `snapshot::snapshot_source` — refuses first (in-progress operation, dirty
  submodule), then reads `pre_status` via `git::inspect::dirty_summary`,
  selects paths to capture, and runs the temp-index sequence
  (`GIT_INDEX_FILE` → `read-tree HEAD` → `add` selected paths → `write-tree`
  → `commit-tree` with source `HEAD` as parent → `update-ref` onto
  `TIDEPOOL_SNAPSHOT_REF_PREFIX/<worktree_id>`). Every temp-index invocation
  goes through `GitCli::with_env`, never mutating the passed-in `GitCli`. The
  temp index file is removed after `write-tree`.
- Submodule dirtiness detection (`refuse_dirty_submodules`, private to
  `snapshot.rs`, STEP 3's "L2's own job" per the lane README): parses
  `git submodule status`. `-` (uninitialized) is skipped, `U` (conflict
  inside the submodule) refuses immediately, `+`/` ` (initialized) run
  `git status --porcelain` **inside the submodule's own working tree** —
  confirmed by experiment that `git submodule status`'s leading character
  reflects only whether the checked-out commit differs from the recorded
  gitlink, never whether the submodule's own working tree is dirty; the two
  axes are independent and must be checked separately. A `+` submodule that
  is otherwise clean is returned as a path to capture explicitly, so a clean
  gitlink bump is captured even if `pre_status` never mentions submodules.
- `captured_paths` = sorted, deduplicated union of `pre_status.staged`,
  `pre_status.unstaged`, `pre_status.untracked`, and any clean-but-bumped
  submodule paths.

## Blocked on L1: `git::inspect::dirty_summary`

Per the lane README, `dirty_summary` is L1's function and L2 must not
implement it. It is still `todo!("L1")` in this worktree. `snapshot_source`
calls it unconditionally right after the refuse-first checks (for
`pre_status`), so **every call that gets past refusal panics today** with
`not yet implemented: L1` — this includes every test but the three
`refuses_*` ones.

I verified `snapshot_source` and every test's assertions are correct
independent of this blocker: I temporarily replaced the `todo!()` body with a
minimal local stub (`diff --cached --name-only` / `diff --name-only` /
`ls-files --others --exclude-standard` (+ `--ignored` for the count)), ran the
full suite, confirmed all 7 tests pass, then reverted `git.rs` via
`git checkout --` back to its frozen `todo!("L1")` state before committing
anything. That revert is confirmed clean (`git diff --stat tidepool-worktree/src/git.rs`
is empty). Nothing under L1's ownership is changed in this branch.

**Action for whoever folds this lane against L1's landed `dirty_summary`:**
just re-run `cargo nextest run -p tidepool-worktree` — no code change should
be needed in `snapshot.rs` or `dirty_snapshot.rs`.

## Test results (per-binary counts, not exit codes)

`cargo nextest run -p tidepool-worktree --no-fail-fast`: **7 tests run, 4
passed, 3 failed** — all 3 failures are the `not yet implemented: L1` panic
above, nothing else.

Passing today (do not depend on `dirty_summary`, since refusal happens before
it is ever called):

- `refuses_when_source_is_mid_merge_and_leaves_it_untouched`
- `refuses_when_source_is_mid_rebase_and_leaves_it_untouched`
- `refuses_dirty_submodule_and_leaves_source_untouched`
- `scaffold_smoke::scripted_writer_drives_a_real_repository` (pre-existing)

Blocked on L1, verified correct via the temporary stub above:

- `dirty_source_untouched_after_snapshot`
- `snapshot_commit_content_matches_captured_working_tree`
- `snapshot_ref_lives_outside_refs_heads_and_never_in_branch_list`

`cargo check --workspace`, `cargo clippy -p tidepool-worktree --all-targets`,
and `cargo fmt --all -- --check` are all clean (the two pre-existing
`dead_code` warnings in `create.rs`/`monitor.rs` are L1/L3's stub fields, not
mine).

## Which test proves which untouched-source property

`assert_source_untouched(before, after)` in `dirty_snapshot.rs` is the shared
helper; it asserts properties 1–6 and 8 as separate `assert_eq!` calls, each
with a message naming what moved. It is called by all six `dirty_snapshot.rs`
tests (both the three blocked-on-L1 success-path tests and the three
refusal tests, so refusal is proven to leave the source untouched too).
Property 7 needs the receipt, so it is asserted directly in the calling test
rather than in the shared helper.

1. **Checked-out branch and `HEAD` unchanged** — `assert_source_untouched`'s
   `before.branch == after.branch` / `before.head == after.head`. Proven by
   every test that calls it.
2. **Ordinary `.git/index` byte-identical** — `assert_source_untouched`'s
   `before.index_bytes == after.index_bytes` (raw `std::fs::read` of
   `.git/index`, not a `git status` comparison — this is the check that would
   catch a snapshot that accidentally touched the real index even if `git
   status` looked unchanged).
3. **Staged content unchanged** — `assert_source_untouched`'s
   `before.diff_cached == after.diff_cached` (`git diff --cached`).
4. **Unstaged content unchanged** — `assert_source_untouched`'s
   `before.diff == after.diff` (`git diff`).
5. **Working-tree bytes and mode bits unchanged** —
   `assert_source_untouched`'s `before.fingerprint == after.fingerprint`,
   via `testing::fingerprint::working_tree`.
6. **Untracked files still untracked, unmodified** —
   `assert_source_untouched`'s `before.untracked == after.untracked`, derived
   from `git status --porcelain`'s `??` lines (bytes covered by property 5's
   fingerprint on the same paths).
7. **Ignored files still ignored, present, not captured** — asserted directly
   in `dirty_source_untouched_after_snapshot`: `ignored.txt` still exists on
   disk, `git check-ignore ignored.txt` still succeeds, and
   `receipt.captured_paths` does not contain it.
8. **No new branch, no new `HEAD` reflog entry** —
   `assert_source_untouched`'s `before.branch_list == after.branch_list`
   (`git branch --list`) and `before.reflog == after.reflog`
   (`git reflog show HEAD`).

Additional proofs beyond the eight, per the STEPS:

- **Snapshot content is right, not just harmless** —
  `snapshot_commit_content_matches_captured_working_tree`: checks out the
  snapshot commit into a scratch `git worktree`, compares
  `fingerprint::working_tree` for every captured path (bytes + mode) against
  the source, separately verifies the submodule gitlink in the snapshot tree
  points at the submodule's actual checked-out commit (fingerprint doesn't
  cover gitlinks, so this needed its own `ls-tree` check), and asserts
  `ignored.txt` is absent from the checkout.
- **Ref namespace** —
  `snapshot_ref_lives_outside_refs_heads_and_never_in_branch_list`: the ref
  starts with `TIDEPOOL_SNAPSHOT_REF_PREFIX`, does not start with
  `refs/heads/`, never appears in `git branch --list`, and the snapshot
  commit is unreachable from any branch (`git branch --list --contains`).
- **Clean submodule works** (STEP 3): `build_messy_repo`'s fixture includes a
  submodule whose checkout was advanced (clean gitlink bump) alongside all
  the other messiness; `dirty_source_untouched_after_snapshot` asserts `sub`
  is in `captured_paths`, and `snapshot_commit_content_matches_captured_working_tree`
  verifies the captured gitlink SHA is correct. `refuses_dirty_submodule_and_leaves_source_untouched`
  uses the same fixture plus an uncommitted edit inside `sub` to prove the
  *dirty* case is distinguished from the *clean-but-bumped* case, which
  turned out to require checking `git status --porcelain` inside the
  submodule directly — `git submodule status`'s prefix alone does not
  distinguish them (see the "What landed" section above).

## For root / other lanes

- No frozen-scaffold file was touched (`id.rs`, `error.rs`, `git.rs`,
  `GitCli` are all untouched — confirmed via `git diff --stat` before commit).
- L1: when `dirty_summary` lands, no change should be needed here; if you'd
  like independent confirmation, re-run
  `cargo nextest run -p tidepool-worktree --no-fail-fast` after it lands —
  it should go from 4/7 to 7/7 with no edits to `snapshot.rs` or
  `dirty_snapshot.rs`.
- `create.rs::WorktreeManager::create`'s dirty branch (L1's job per the lane
  split) can call `snapshot::snapshot_source(&self.git, &self.source_repository,
  &worktree_id, <a temp dir outside the source tree>)` directly; the
  signature is unchanged from the frozen scaffold.
