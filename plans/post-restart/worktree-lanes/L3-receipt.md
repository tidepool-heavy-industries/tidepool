# L3 receipt — event monitor (poll/reconcile, journal)

## What landed

- `tidepool-worktree/src/journal.rs` — `EventJournal`: append-only JSONL, one
  row per line, outside the source tree (caller chooses the path — same
  discipline as `WorktreeRegistry::open`). `append` opens the file in append
  mode, writes one line, `fsync`s, and closes, before returning the new
  cursor. `open` reads every line and skips (with an `eprintln!` diagnostic,
  never a panic) any line that fails to parse as JSON — this covers a torn
  final row from a crash mid-`writeln!`, and more generally any corrupted
  line, not just the last one. `since(cursor)` filters the in-memory row list;
  `end_cursor()` is the last row's `cursor`, or `0` for an empty journal.
  Filesystem errors on `open`/`append` (missing directory, full disk) panic
  with a clear message rather than going through `WorktreeError` — see
  "Frozen-contract friction" below.

- `tidepool-worktree/src/monitor.rs` — `WorktreeMonitor`: `register(id, path)`
  establishes a worktree's starting baseline (from the journal's last
  observation of that id if one exists — restart recovery — otherwise a fresh
  git read), then `reconcile(&id)` diffs current git state against that
  baseline, classifies the movement, mints one `EventId` for the pass, builds
  and journals the observations, and advances the baseline. `WorktreeMonitor`
  owns one `EventJournal` shared across every worktree it watches (cursor
  semantics — no-replay, restart recovery — are defined against one journal
  end, not one per worktree).

- `tidepool-worktree/tests/event_monitor.rs` — 14 tests (13 new + the
  pre-existing `scaffold_smoke` test), all against real `tempfile` git repos
  via `TestRepo`/`ScriptedWriter`. Per-binary count: `cargo nextest run -p
  tidepool-worktree` → **14 tests run: 14 passed, 0 skipped**, across the
  `tidepool-worktree` lib-test binary and the `event_monitor` /
  `scaffold_smoke` integration-test binaries.

## Classification arms, and the test proving each

| `HeadChangeKind` | Test | How it's produced / detected |
|---|---|---|
| `Advanced` (single commit) | `commit_yields_commit_and_head_changed_sharing_one_event_id` | `old` is an ancestor of `new` (`merge-base --is-ancestor`); commits gained via `git log --reverse old..new`. |
| `Advanced` (coalesced) | `two_commits_between_polls_coalesce_into_one_advanced` | Same check; two writer commits between polls still produce exactly ONE `HeadChanged`/`Advanced` carrying both oids, oldest first. |
| `Amended` | `amend_yields_amended_and_a_commit_for_the_new_tip` | `git rev-list --parents -n1` on old and new tips have the SAME parent set — checked first, before any ancestor walk, because it's a precise signature independent of whether the message also changed. |
| `Rewound` | `reset_hard_backwards_yields_rewound_with_no_commit` | `new` is an ancestor of `old` (reverse of the `Advanced` check). |
| `Switched` | `branch_checkout_yields_switched_with_no_commit` | The branch name (`symbolic-ref --short HEAD`) differs from the baseline's — checked FIRST, before any oid-based classification, since a branch identity change is the most certain fact available and takes priority over what the oid also did. |
| `Rewritten` | `rebase_onto_yields_rewritten_with_matched_pairs` | Neither ancestor check holds; commits unique to `old` are matched, in order, to same-subject commits unique to `new` (`match_rewritten`), tolerating extra upstream-only commits interleaved — the honest shape of "a rebase preserves messages, changes parents". |
| `UnknownChange` (disjoint history, same branch) | `unrelated_history_on_the_same_branch_yields_unknown_change_rather_than_a_guess` | Neither ancestor check holds AND the subject-matching heuristic fails (no shared messages) — degrades rather than forcing a `Rewritten` match or inventing `Advanced`. |
| `UnknownChange` (unresolvable object — "the common trap") | `unreachable_old_head_after_gc_yields_unknown_change` | Old head is `reflog expire`d + `git gc --prune=now`d out of the odb after being captured as a baseline. `merge-base --is-ancestor` then fails to resolve it at all (not a `1`/"not an ancestor" exit); `is_ancestor` treats that non-`1` failure as `Ancestry::Unknown`, distinct from a definitive `No`, and `classify` returns `UnknownChange` immediately rather than falling through to a Rewritten-match attempt against data it can't actually read. |

`real_git_worktree_add_is_monitored_directly` additionally proves the monitor
works unmodified against an actual `git worktree add` linked worktree, not
just a `TestRepo`'s primary checkout (used for every other scenario test,
since nothing in the classifier or git reads depends on linked-vs-primary).

## Design decisions this lane made (spec explicitly left them to me)

**First observation emits nothing.** `WorktreeMonitor::register` eagerly
establishes a concrete baseline (from the journal, or else a fresh git read)
before `reconcile` is ever called. The very first `reconcile` after
`register` therefore compares current state to an already-accurate baseline
and — absent writer activity in between — returns empty, exactly like any
other idempotent no-op poll. No `HeadChanged` with `old_head: None` is ever
constructed by this implementation. Reasoning: there is no prior state for
the worktree to have moved FROM, honest classification has no `HeadChangeKind`
that means "first sight", and the no-replay rule already guarantees a handler
that subscribes after this priming pass never sees it regardless of which way
I'd chosen. `first_reconcile_after_register_establishes_baseline_without_emitting`
is the dedicated test; `reconciling_with_no_writer_activity_is_idempotent`
covers the general idempotence property (first reconcile AND repeated
reconciles after real activity both settle to empty).

**`Commit` is emitted per honestly-inferable commit, not once per pass.** For
a coalesced `Advanced` carrying N gained commits, this implementation emits N
`Commit` observations (one per commit, each individually built from real git
metadata) alongside the single `HeadChanged`/`Advanced`, all sharing the
pass's one `EventId`. The acceptance wording "two commits... coalesce into
ONE Advanced... not two events" is about `HeadChanged` staying a single
coalesced delta (which it does — see `two_commits_between_polls_coalesce_into_one_advanced`);
it says nothing about the `commit` stream, which the PRD frames as a
different-job signal ("high-signal... for review, test, and receipt
reactions") that should not silently swallow a real commit just because the
poll was slow to catch up. Each commit in the range is just as honestly
inferable as a single one — this is a deliberate reading of "honest
inference", not an ambiguity I left unresolved, and it's flagged here for
root/sibling lanes to override if they disagree.

**`Commit` policy by kind:** `Advanced` → one `Commit` per gained commit.
`Amended` → one `Commit` for the replacement tip (the PRD's own "commit...
serves review, test, receipt reactions" section lists amend as one of the
cases `commit` should fire for). `Rewound`, `Switched`, `Rewritten`,
`UnknownChange` → no `Commit` (no new commit object was created by a reset or
checkout, and a rebase's synthetic commits are not "a commit the user made in
this worktree" — `Rewound`/`Switched` are proven commit-free in their
dedicated tests; the rebase test asserts the same).

## Poll/reconcile interval — root ruling, 2026-08-08

Root explicitly assigned this decision to L3 rather than anchoring on
Exomonad's 15s inbox backstop (different urgency profile). Landed as
`tidepool_worktree::DEFAULT_POLL_INTERVAL_MS: u64 = 5_000` (5 seconds),
exported from `monitor.rs` with its reasoning inline so a future driver can
override it rather than burying a literal in whatever loop eventually calls
`reconcile` on a schedule (no such loop exists in this crate — `reconcile` is
a single pass, driven externally, per the crate's "no effects, no JIT, no
Haskell, no agents" scope).

Reasoning: the consumer of a `headChanged` poke is a child agent deciding
whether to rebase, not a human waiting on a spinner, and agent work (write,
test, commit) runs on a cadence of tens of seconds to minutes — a few seconds
of propagation staleness is imperceptible against that. A no-op reconcile
costs two constant-time git invocations per worktree (`rev-parse HEAD`,
`symbolic-ref`), a few ms of process-spawn overhead each regardless of
repository size, so the number doesn't need to be conservative for cost
reasons even across dozens of watched worktrees. 5s reads as near-immediate
against agent cadence without fleet size ever being a reason to widen it —
revisit against real dev-tree telemetry if that changes.

Per root's clarification, the notify+backstop property itself (hooks wake,
reconciliation decides, polling is the fallback) is NOT an obligation of this
lane — PRD 19 already structurally mandates it as acceptance criterion 8, and
its test lands with the hook-adapter lane. This lane stays polling-only, with
no hook adapter and no notify path, as instructed.

## Frozen-contract friction (flagging, not changing)

`WorktreeError` (frozen, owned by scaffold) has no variant for "the journal
file itself could not be written" — every variant is about git or worktree
domain state, not local filesystem I/O. `EventJournal::open`/`append` treat a
genuine filesystem failure (missing directory permissions, disk full) as an
environment failure and panic with a clear message, the same way
`testing::TestRepo` (also frozen) already treats its own `TempDir`/`fs` setup
failures via `.expect(...)`. I did not add an `Io` variant to `WorktreeError`
myself, per the lane README's instruction to flag rather than change the
frozen contract — if a future lane wants journal I/O failures to be
catchable/case-matchable rather than a panic, that needs a
`WorktreeError::Io`-shaped variant added by agreement, not by me here.

## What was stubbed around

L1 (`WorktreeRegistry`/`WorktreeManager`) has not landed. `WorktreeMonitor`
was designed and tested to need only a `WorktreeId` plus a `PathBuf` — supplied
directly in every test, either as a `TestRepo`'s own working tree (13 of 14
tests) or a real `git worktree add` linked worktree
(`real_git_worktree_add_is_monitored_directly`, to confirm the "any git
working directory, linked or not" assumption holds). Nothing here imports or
assumes anything about `registry.rs`/`create.rs`/`binding.rs`. When L1 lands,
its `WorktreeHandle`/`WorktreeSummary` should be able to hand a monitor
`(worktree_id, cwd)` directly — no change to `monitor.rs`'s public surface is
anticipated, but that's L1's call to confirm at fold.

## For other lanes / root

- `EventJournal`/`WorktreeMonitor`/`DEFAULT_POLL_INTERVAL_MS` are exported
  from `tidepool-worktree`'s crate root (`src/lib.rs`) alongside the other
  monitor types.
- `WorktreeMonitor::new(git, journal)` takes ownership of the `EventJournal`
  (one journal per monitor, shared across every watched worktree) rather than
  the scaffold's original `new(git: GitCli)` — the stub had no way to satisfy
  "durable before dispatch" or "survive restart via the journal" without
  holding one, so this is a signature change within a file this lane fully
  owns, not a frozen-contract change.
- No hook adapter, no `workspaceOf`, no agent types, no git workflow verbs —
  none were touched, per the lane's HOLD lines.
