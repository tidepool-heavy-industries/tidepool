# L8 receipt — seam fixes on the landed event monitor

Follow-up lane on `tidepool-worktree/src/monitor.rs`. Two defects found by the
surface lane (L4) building the effect adapter on top of the landed L3
substrate: a panic on an unregistered worktree, and a minted `EventId` that
never reached the caller. Base commit: `2db7471a`
("docs(worktree-lanes): envelope rule — one brokered leg per dev; drain,
don't kill"), `root.worktree-wave`'s tip at the time this lane started.

## Defect 1 — panic on an unregistered worktree

`monitor.rs::reconcile` did:

```rust
let baseline = self.baselines.get(worktree).unwrap_or_else(|| {
    panic!("tidepool-worktree: reconcile called for unregistered worktree {worktree} …")
});
```

Worktree ids reach `reconcile` from author-supplied values at the effect
surface, so a resident naming an unregistered id aborted the whole eval by
panic instead of receiving a typed failure it could match on — exactly the
authored boundary the typed-`WorktreeError` contract exists to protect.
`WorktreeError::WorktreeNotRegistered(WorktreeId)` already existed for this
condition (used elsewhere by `create.rs`'s dirty-source lookup path); nothing
new was added, only wired in:

```rust
let baseline = self
    .baselines
    .get(worktree)
    .ok_or_else(|| WorktreeError::WorktreeNotRegistered(worktree.clone()))?;
```

## Defect 2 — the journalled `EventId` was unreachable

`reconcile` minted one `EventId` per pass, journalled every emitted row under
it, then returned `Result<Vec<RepositoryEvent>, WorktreeError>` — carrying no
id at all. A caller (or the surface adapter above this crate) could not
correlate an event it received back to its journal row, defeating the
journal's stated purpose (traceability and restart diagnosis).

`Observed<T> { event_id: EventId, value: T }` was already defined and
exported from `monitor.rs`, designed for exactly this and never wired
through. `reconcile`'s signature changed to return it instead of inventing a
parallel type or a positional tuple:

```rust
pub fn reconcile(&mut self, worktree: &WorktreeId)
    -> Result<Vec<Observed<RepositoryEvent>>, WorktreeError>
```

Every `events.push(ev)` at the three emission sites (per-gained-commit in
`Advanced`, the `Amended` tip commit, and the final `HeadChanged`) became
`events.push(Observed { event_id, value: ev })`, using the SAME `event_id`
local already passed to `self.journal.append(&ev, event_id)` two lines above
— the returned id is the journalled id by construction, not by a second
lookup that could drift from it.

In-crate consumers updated: `tidepool-worktree/tests/event_monitor.rs`'s
`head_changed`/`commits` helpers now take `&[Observed<RepositoryEvent>]` and
filter on `.value`; the one place a test matched a bare `RepositoryEvent`
(`commit_yields_commit_and_head_changed_sharing_one_event_id`) now matches
`events[0].value`. No other in-crate module calls `reconcile` (checked by
grep across `src/` and `tests/` — only `journal.rs`'s `RepositoryEvent`
import and `create.rs`'s unrelated `WorktreeNotRegistered` use turned up).

## Panic/abort audit of `monitor.rs`

Every `panic!`/`.unwrap()`/`.expect()` in the file, verdict and reasoning:

| Location | Verdict | Reasoning |
|---|---|---|
| `reconcile`'s baseline lookup (was line 273) | **Fixed** — converted to `WorktreeNotRegistered` | Defect 1, above. |
| `baseline.head.clone().expect(...)` (was line 283, an `Advanced`/`Amended` build path) | **Unreachable-by-construction, kept** | `baselines` is populated ONLY by `register`, and `register` never inserts a `Baseline` with `head: None` — either `last_observed` recovers a `Some` from the journal, or the fresh-read branch assigns `head = Some(read_head(...)?)` before insertion (and now returns `Err` before that read if the path is gone, per the task-3 decision below — see next section). There is no code path that stores `head: None`, so every baseline `reconcile` can reach already carries `Some`. |
| `new()`'s `EventJournal::since(0).expect("EventJournal::since never fails")` | **Unreachable-by-construction, kept** | `EventJournal::since` (`journal.rs`) is `pub fn since(&self, cursor: u64) -> Result<Vec<JournalEntry>, WorktreeError> { Ok(...) }` — it always constructs `Ok`, unconditionally, for every input including `0`. The `Result` return type exists for API-shape consistency with the rest of the crate, not because this call can fail. |
| `last_observed`'s identical `.expect("EventJournal::since never fails")` | **Unreachable-by-construction, kept** | Same reasoning as above; same call. |
| `.unwrap_or((None, None))` at the end of `last_observed` | **Not an abort risk** | `Option::unwrap_or` on a `find_map` result — supplies the "no prior observation" default, never panics regardless of input. |
| `parts.next().unwrap_or_default()` / `unwrap_or("0")` in `commits_only_in`/`build_commit_receipt` | **Not an abort risk** | All `Option::unwrap_or*` combinators over `str::split`/`splitn` iterators, each supplying an empty-string/zero default rather than panicking on a short line. Pre-existing, untouched, out of scope for this audit's "panic" class since none of them can abort. |

Net: one true panic fixed (Defect 1), one sibling `.expect` audited and
confirmed genuinely unreachable rather than converted, two more `.expect`s
in the same "journal read can't fail" family confirmed for the same reason.
No other abort risk in the file.

## Task 3 — does `register` have the same exposure, and should a
## since-removed worktree reconcile as `WorktreeLost`?

**Decision: yes to both, and both were fixed — not scope creep, because it
is the identical failure class as Defect 1 wearing a different mask: an
opaque, unmatchable failure standing in for a condition the crate already
has a typed name for.**

Before this fix, if a worktree's directory was removed from disk after
`register` (a human deleting it — the exact "retain first, never silently
recreated" scenario `WorktreeError::WorktreeLost` exists to name) —
`reconcile`'s subsequent `read_head(&self.git, &path)` would shell out `git
rev-parse HEAD` against a nonexistent `cwd`. `GitCli::run` maps that to a
`GitFailureReceipt` with `exit_code: None` and `stderr: "spawn failed: No
such file or directory (os error 2)"` (or, if the directory happens to still
exist but is no longer a working tree, a `NotARepository`-shaped condition
git itself rejects) — an opaque `WorktreeError::GitFailure` an author has to
pattern-match on stderr text to recognize, rather than the
`WorktreeError::WorktreeLost(WorktreeId)` the crate already uses for this
exact condition everywhere else: `create.rs`'s `WorktreeManager::lookup` and
`WorktreeManager::worktree_head` both check `registry::worktree_present`
first and return `WorktreeLost` before touching git. `reconcile` was the one
call site in the crate reading a worktree's git state that skipped this
check.

`register`'s fresh-baseline branch (the "no journal recovery" case) has the
identical shape: if `head.is_none()`, it calls `read_head`/`read_branch`
against `path` with no existence check first. It cannot panic (there is no
`.unwrap`/`.expect` on that path), so it never shared Defect 1's *abort*
exposure — but it shared the *opaque-failure* exposure the task's framing
asks about, so it got the same fix.

Both sites now guard with the crate's existing `pub(crate)
registry::worktree_present` helper — the same one `lookup`/`worktree_head`
use, not a restated check:

```rust
// register(), fresh-baseline branch
if !crate::registry::worktree_present(&path) {
    return Err(WorktreeError::WorktreeLost(worktree));
}

// reconcile(), before any git read
if !crate::registry::worktree_present(&baseline.path) {
    return Err(WorktreeError::WorktreeLost(worktree.clone()));
}
```

**What stayed out of scope, deliberately:** the CLASSIFICATION logic
(`Advanced`/`Amended`/`Rewritten`/`Rewound`/`Switched`/`UnknownChange`),
which already has its own honest-degradation discipline (`Ancestry::Unknown`
→ `UnknownChange`) for git-level ambiguity distinct from "the directory is
gone" — that is a different condition with a different existing answer, and
this fix does not touch it. Nothing in `create.rs`/`registry.rs`/`binding.rs`
was touched; `worktree_present` was consumed, not redefined or relocated.

## Named gates

Per `GATES.md`: each gate below passes BY NAME, run in isolation (`cargo
nextest run -p tidepool-worktree -E 'test(=<name>)'`), not only inside the
aggregate. Instrument: `cargo-nextest` (per-test PASS line + summary count),
same instrument as every other lane receipt in this directory.

```
$ cargo nextest run -p tidepool-worktree -E 'test(=reconcile_on_unregistered_worktree_returns_worktree_not_registered)'
        PASS [   0.007s] (1/1) tidepool-worktree::event_monitor reconcile_on_unregistered_worktree_returns_worktree_not_registered
     Summary [   0.008s] 1 test run: 1 passed, 52 skipped

$ cargo nextest run -p tidepool-worktree -E 'test(=reconcile_of_a_worktree_removed_from_disk_returns_worktree_lost)'
        PASS [   0.035s] (1/1) tidepool-worktree::event_monitor reconcile_of_a_worktree_removed_from_disk_returns_worktree_lost
     Summary [   0.035s] 1 test run: 1 passed, 52 skipped

$ cargo nextest run -p tidepool-worktree -E 'test(=reconcile_returned_event_id_matches_the_journalled_event_id_for_that_pass)'
        PASS [   0.108s] (1/1) tidepool-worktree::event_monitor reconcile_returned_event_id_matches_the_journalled_event_id_for_that_pass
     Summary [   0.108s] 1 test run: 1 passed, 52 skipped

$ cargo nextest run -p tidepool-worktree -E 'test(=commit_yields_commit_and_head_changed_sharing_one_event_id)'
        PASS [   0.061s] (1/1) tidepool-worktree::event_monitor commit_yields_commit_and_head_changed_sharing_one_event_id
     Summary [   0.062s] 1 test run: 1 passed, 52 skipped
```

What each proves, and its wrong-reason guard (per `GATES.md`'s "ask what ELSE
would make this pass"):

- **`reconcile_on_unregistered_worktree_returns_worktree_not_registered`**
  (Defect 1's gate) — `reconcile` on an id `register` never ran for. Asserts
  BOTH that it errors AND that the error is specifically
  `WorktreeNotRegistered` carrying the SAME id (`matches!(&err,
  WorktreeError::WorktreeNotRegistered(bad) if bad == &id)`), not merely "some
  error came back" — a mislabeled or generic failure would fail the `matches!`
  guard even though `reconcile` still returned `Err`.

- **`reconcile_of_a_worktree_removed_from_disk_returns_worktree_lost`**
  (task 3's decision) — registers and primes against a real repo, removes the
  directory by hand, then asserts `WorktreeLost(id)` specifically, the same
  guard shape as above, distinguishing this from Defect 1's gate: one
  proves the id was *never* registered, the other proves it *was* registered
  and has since gone missing — two different `WorktreeError` variants that
  must not collapse into each other.

- **`reconcile_returned_event_id_matches_the_journalled_event_id_for_that_pass`**
  (Defect 2's load-bearing gate) — this is the one the task explicitly
  flagged as needing a wrong-reason guard, because a naive version ("some id
  came back, and the journal has some id too") would pass under an
  implementation that mints a disconnected id at the return path, exactly the
  bug being fixed. The gate:
  1. Runs TWO separate reconciliation passes and asserts their returned ids
     are DISTINCT (`assert_ne!`). This alone kills "always return
     `EventId(0)`" or any other constant/shared-id implementation — a
     hardcoded return value cannot vary across passes.
  2. Asserts every `Observed` in the second pass carries that SAME id (the
     one-id-per-pass invariant, checked at the return type directly, not
     inferred from the journal).
  3. Re-opens the journal and filters its rows to exactly the second pass's
     returned id, then asserts the COUNT of matching journal rows equals the
     COUNT of returned events (closes "the journal recorded nothing" — an
     empty filter fails this immediately) AND that every returned event's
     VALUE appears among those rows (closes "the ids coincidentally matched
     but the content is unrelated").
  Any implementation that mints the returned id independently from the
  journalled one — the exact shape of Defect 2 — fails step 1 (if the
  independent id is a constant) or step 3 (if it varies but by a different
  scheme than the journal's, so counts or content diverge).

- **`commit_yields_commit_and_head_changed_sharing_one_event_id`**
  (pre-existing test, extended, not renamed) — proves co-emitted `Commit` +
  `HeadChanged` still share one id after the signature change, at THREE
  levels rather than only the journal-only check it had before: the
  `Observed` values `reconcile` returns share one id
  (`events[0].event_id == events[1].event_id`, new), the journal rows for
  this pass share one id (`ids[0] == ids[1]`, pre-existing), AND the returned
  id equals the journalled id (`events[0].event_id == ids[0]`, new — this is
  the specific link Defect 2 was missing). The pre-existing assertions
  (`Commit` before `HeadChanged` observation order, receipt field values,
  `HeadChangeKind::Advanced`) are untouched.

## Full-suite verification

Instrument: `cargo-nextest`; the `Summary` line's "N tests run" is nextest's
own completed count, cross-checked against "N of N" in the per-test progress
lines above it (per the box-wide fail-fast-truncation advisory: this run's
`-p tidepool-worktree` scope never reaches `tidepool-runtime`, the crate
carrying the live inherited red, so it is not subject to that hazard — but
stating completed-vs-total rather than only the pass count is now the
standing habit regardless).

```
$ cargo nextest run -p tidepool-worktree
     Summary [   4.182s] 53 tests run: 53 passed, 0 skipped
```

53 of 53 completed (COMPLETED == CRATE TOTAL). 53 = the 50 pre-existing tests
(all unedited in substance — the only touches to `tests/event_monitor.rs`'s
pre-existing tests were the two mechanical `Observed`-shape adaptations
required by Defect 2's authorized signature change: the `head_changed`/
`commits` helper signatures, and one bare `RepositoryEvent` match becoming
`events[0].value`; no assertion, expected value, or test name changed) plus
4 new tests in `tests/event_monitor.rs` (3 wholly new gates above, plus the
one pre-existing test extended with 2 additional assertions rather than
counted as new).

```
$ cargo check -p tidepool-worktree
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.81s   # clean

$ cargo clippy -p tidepool-worktree --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.42s   # clean, no warnings

$ cargo fmt --all -- --check
    # clean, rc=0 (one prior run flagged two formatting nits introduced by this
    # lane's own test edits — a multi-line fn signature and a long filter/collect
    # chain in tests/event_monitor.rs; `cargo fmt -p tidepool-worktree` applied
    # rustfmt's own normalization for both before this final --check run)
```

## Scope discipline

- Touched: `tidepool-worktree/src/monitor.rs` (both fixes + task-3 decision),
  `tidepool-worktree/tests/event_monitor.rs` (mechanical shape adaptation +
  4 new/extended tests).
- Untouched: `monitor.rs`'s classification logic, `snapshot.rs`,
  `registry.rs`/`binding.rs` (consumed `worktree_present`, did not modify
  it), `git.rs`, `create.rs`, `id.rs`, `error.rs` (no new variant — both
  fixes reuse `WorktreeNotRegistered`/`WorktreeLost`, which already existed).
- No `haskell/` or `tidepool-handlers/` file touched or referenced.

## For L4

The surface lane's adapter hardened around both defects rather than reaching
into this crate — the correct call, and the reason this lane exists. With
`reconcile` now returning `Vec<Observed<RepositoryEvent>>` carrying the
journalled `EventId` directly, and `WorktreeNotRegistered`/`WorktreeLost`
surfacing as typed `Err`s instead of a panic or an opaque `GitFailure`, L4
can drop whatever workaround it built to (a) survive the panic path and (b)
mint its own disconnected id for `Observed.eventId` at the effect surface —
the id `reconcile` now returns IS the journalled one, so no re-derivation is
needed there anymore.
